//! The sync orchestration, written once against [`SyncBackend`] (#0059).
//!
//! Everything here used to live inside `imap_client::store_sync::sync_mailboxes`
//! between the network calls, which is why none of it had a test: driving it
//! meant standing up an IMAP server. The transport is now a trait parameter, so
//! the loop below runs offline against a fake backend and the properties it is
//! responsible for, the arrival mark, the ingest-failure bound, the flag
//! application, the cursor, and the deferred prune pass, are pinned through the
//! real code path rather than re-walked by hand in a composition test.
//!
//! What it is *not*: a Graph orchestrator. `graph.rs` still runs its own loop
//! (the parity half of #0059 is parked with the Graph backend), and its #0074
//! bookkeeping mirrors the one here by hand as it did before. The #0115
//! convergence detector rides this loop only, so a Graph pass builds a
//! `SyncResult` whose `non_converging` is always empty: the account that
//! motivated it is IMAP, and the Graph half waits for the parked parity work
//! rather than being mirrored by hand a second time.

use std::hash::{DefaultHasher, Hash, Hasher};

use anyhow::Result;
use log::{info, warn};

use super::{FreshObservation, MailboxFetch, SyncBackend, SyncResult, SyncTarget};
use crate::ingest::{self, IngestInput, MailboxCursor, RebindPolicy};
use crate::parse::parse_rfc822_to_fetched_email;
use crate::store::{schema, BlobStore, Store};
use crate::timing::TimingSpan;
use crate::types::MailboxRole;

/// Prefix of the per-mailbox `meta` row the non-convergence detector keeps
/// (#0115). The store is per account (`config::store_path`), so the mailbox
/// role is the whole key, as it is for every other per-mailbox marker here.
const NONCONVERGING_PREFIX: &str = "nonconverging:";

fn nonconverging_key(role: &str) -> String {
    format!("{NONCONVERGING_PREFIX}{role}")
}

/// The identity of one pass's download: the UID set, order-independent, plus
/// its size so a hash collision still has to agree on the count.
fn fingerprint(uids: &[u32]) -> (u64, usize) {
    let mut sorted = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut hasher = DefaultHasher::new();
    sorted.hash(&mut hasher);
    (hasher.finish(), sorted.len())
}

/// The whole state machine of #0115: how many consecutive qualifying passes
/// have now downloaded the same UID set.
///
/// `prev` is the persisted `(hash, count, streak)`, `now` this pass's
/// fingerprint. The same fingerprint continues the streak, anything else
/// starts a new one at 1. Pure, so the transition is testable without a store.
fn advance_streak(prev: Option<(u64, usize, u32)>, now: (u64, usize)) -> (u64, usize, u32) {
    match prev {
        Some((hash, count, streak)) if hash == now.0 && count == now.1 => {
            (now.0, now.1, streak.saturating_add(1))
        }
        _ => (now.0, now.1, 1),
    }
}

/// Which streak lengths get a log line: the first repeat, the second, then
/// every tenth. A bug that persists stays visible without one line per tick.
fn streak_is_loud(streak: u32) -> bool {
    streak == 2 || streak == 3 || (streak > 3 && streak.is_multiple_of(10))
}

fn parse_streak_row(value: &str) -> Option<(u64, usize, u32)> {
    let mut parts = value.split(':');
    let hash = parts.next()?.parse().ok()?;
    let count = parts.next()?.parse().ok()?;
    let streak = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((hash, count, streak))
}

fn clear_nonconverging(store: &Store, role: &str) {
    if let Err(e) = schema::clear_meta(store.conn(), &nonconverging_key(role)) {
        warn!("[sync] could not clear the convergence marker for {role}: {e:#}");
    }
}

/// Record what this pass downloaded for one mailbox and say whether the fetch
/// has stopped converging (#0115).
///
/// Returns the streak once it is at 2 or more, which is the state the status
/// line reports; the log line itself is throttled by [`streak_is_loud`].
///
/// A pass with nothing new clears the row: the fetch converged, which is the
/// whole point of the detector.
fn note_download_fingerprint(
    store: &Store,
    account: &str,
    target: &SyncTarget,
    uids: &[u32],
) -> Option<u32> {
    let key = nonconverging_key(target.role.as_str());
    if uids.is_empty() {
        clear_nonconverging(store, target.role.as_str());
        return None;
    }
    let prev = match schema::get_meta(store.conn(), &key) {
        Ok(v) => v.as_deref().and_then(parse_streak_row),
        Err(e) => {
            warn!("[sync] could not read the convergence marker for {key}: {e:#}");
            return None;
        }
    };
    let (hash, count, streak) = advance_streak(prev, fingerprint(uids));
    if let Err(e) = schema::set_meta(store.conn(), &key, &format!("{hash}:{count}:{streak}")) {
        warn!("[sync] could not record the convergence marker for {key}: {e:#}");
    }
    if streak < 2 {
        return None;
    }
    if streak_is_loud(streak) {
        let min = uids.iter().min().copied().unwrap_or(0);
        let max = uids.iter().max().copied().unwrap_or(0);
        warn!(
            "[sync] '{}' on account '{account}' downloaded the same {count} message(s) \
             (uids {min}..{max}) on {streak} consecutive passes; the fetch is not converging, \
             see docs/tickets/0115-warn-on-a-non-converging-fetch.md",
            target.server_name
        );
    }
    Some(streak)
}

/// Everything one sync pass needs that is not the transport: where to write,
/// what to sync and how much of it.
///
/// A struct rather than six parameters so [`run_sync`] stays readable and the
/// caller cannot swap `account` for `limit` silently.
pub struct SyncRun<'a> {
    pub store: &'a Store,
    pub blobs: &'a BlobStore,
    pub account: &'a str,
    pub targets: &'a [SyncTarget],
    /// How many of the newest UIDs per mailbox the pass may download.
    pub limit: usize,
    /// Count what would be ingested without touching the store or the blobs.
    pub dry_run: bool,
}

/// The IMAP half of [`ingest::note_ingest_failure`]: the same give-up bound,
/// over this path's `u32` UIDs. `false` means the arrival mark no longer has to
/// stay below the UID (#0074).
fn note_ingest_failure(
    store: &Store,
    account: &str,
    mailbox: &str,
    server_name: &str,
    uid: u32,
    error: &str,
) -> bool {
    ingest::note_ingest_failure(store, account, mailbox, server_name, uid as i64, error)
}

/// The arrival mark a pass must persist once ingest is done, given the mark the
/// download reported and the UIDs the ingest failed to write (#0074).
///
/// The backend's own coverage measures what the pass *downloaded*, which is one
/// step short of what it owes the next pass: a message fetched and then not
/// written is as absent from the store as one never fetched, yet it reads as
/// covered, the pass reports itself complete, persists no mark, and the next
/// pass stands on a floor above a message the server still lists. The mark is
/// therefore lowered here to just under the lowest unwritten UID, which is what
/// makes that message an arrival again next pass and keeps the gate shut until
/// some pass writes it.
///
/// `unmet` holds only the failures still worth retrying;
/// [`ingest::note_ingest_failure`] drops a UID out of it once it has failed
/// [`ingest::MAX_INGEST_ATTEMPTS`] times, so a message the store rejects
/// deterministically cannot hold the mark down for good.
///
/// Saturating at 0 rather than wrapping: a UID of 1 that will not ingest leaves
/// a mark of 0, meaning every listed UID is an arrival, which is the correct
/// reading when the very bottom of the mailbox is missing.
pub(crate) fn mark_below_unmet(pending: Option<u32>, unmet: &[u32]) -> Option<u32> {
    let Some(lowest) = unmet.iter().copied().min() else {
        return pending;
    };
    let owed = lowest.saturating_sub(1);
    Some(pending.map_or(owed, |mark| mark.min(owed)))
}

/// Drive one sync pass: read the skip lists, hand them to the backend, ingest
/// what comes back in target order, then apply the prunes.
///
/// The phases and their order are load-bearing and unchanged from the
/// pre-#0059 IMAP orchestrator:
///
/// 1. every target's skip list is read from the store serially, before any
///    network call, so the fetch never races the single SQLite writer;
/// 2. the backend fetches, in whatever order it likes, and hands the results
///    back in target order;
/// 3. ingest runs serially in target order, so per-mailbox transactions cannot
///    interleave and the prune ordering below holds;
/// 4. every prune runs after every target has been ingested, gated on the whole
///    pass's coverage.
pub async fn run_sync(
    backend: &mut impl SyncBackend,
    run: &SyncRun<'_>,
    span: &mut TimingSpan,
) -> Result<SyncResult> {
    let SyncRun { store, blobs, account, targets, limit, dry_run } = *run;

    let mut result = SyncResult::default();
    // Every prune this run will apply, collected here and applied after the
    // loop: see the second pass below for why it cannot run per target.
    let mut prunes: Vec<(MailboxRole, Vec<u32>)> = Vec::new();
    // `(enumeration complete, download short)` per target, which decides
    // whether the prunes above may be applied at all (#0072). Mirrors the
    // Graph backend; the shared gate is `ingest::pass_may_prune`.
    let mut coverage: Vec<(bool, bool)> = Vec::with_capacity(targets.len());

    // Phase 1: read the store's skip list for every target, serially. These
    // are single-reader queries and cheap; holding them all in hand lets the
    // network fetch below run without touching the store (single-writer
    // discipline: nothing concurrent reads or writes SQLite).
    //
    // The skip list travels with the UIDVALIDITY it was recorded under, so the
    // fetch can throw it away when the server has renumbered; carrying it
    // across a reset would skip bodies that were never downloaded.
    let mut knowns = Vec::with_capacity(targets.len());
    for target in targets {
        knowns.push(ingest::known_uids_with_cursor(store, account, target.role.as_str())?);
    }

    // Phase 2: the transport. One result per target, in target order, whatever
    // order the backend actually fetched them in (see [`SyncBackend`]).
    let fetched_results = backend.fetch_targets(targets, limit, knowns).await;
    span.mark("fetch");

    // Phase 3: ingest serially, in target order.
    for (target, fetched) in targets.iter().zip(fetched_results) {
        let fetched: MailboxFetch = match fetched {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    "Failed to sync mailbox '{}': {}. Continuing with next.",
                    target.server_name, e
                );
                // A target that did not sync at all is the strongest form of
                // partial pass: the copy that would justify another target's
                // deletion may be exactly what this fetch failed to bring in.
                coverage.push((false, false));
                continue;
            }
        };
        let new_messages = fetched.messages;
        let state = fetched.state;
        let pending_arrival_mark = fetched.pending_arrival_mark;
        // A body pass that stopped at its deadline (#0113) left new mail on the
        // server it never asked for. The backend already reports that through
        // the coverage arithmetic and declines its own modseq; the flag is read
        // here as well, so the two consequences hold whatever a backend fills
        // `download_incomplete` and `highest_modseq` with, and so the status
        // line can say which pass was cut.
        let bodies_complete = fetched.bodies_complete;
        if !bodies_complete {
            result.bodies_truncated += 1;
        }
        result.skipped += fetched.skipped;
        if fetched.uidvalidity_reset {
            result.uidvalidity_resets += 1;
            // The retry counters are keyed by UID, and the server has just
            // renumbered them: every recorded attempt now points at a message
            // that no longer holds that UID, so it is dropped with the mark and
            // the skip list the refetch already discards (#0074 review).
            ingest::clear_mailbox_ingest_failures(store, account, target.role.as_str());
            // The CONDSTORE resume point goes with them, and has to go from
            // here: the cursor UPSERT carries a modseq forward precisely so an
            // ordinary full-window pass cannot erase it (#0041), which leaves
            // this as the one path that may. A modseq recorded under the old
            // UIDVALIDITY describes a mailbox that no longer exists.
            ingest::clear_mailbox_modseq(store, account, target.role.as_str());
            // And so does the convergence marker (#0115): it fingerprints a
            // UID set the server has just renumbered, so the pass after this
            // one would compare two sets that never meant the same thing.
            if !dry_run {
                clear_nonconverging(store, target.role.as_str());
            }
        }

        if dry_run {
            coverage.push((
                fetched.enumeration_complete,
                fetched.download_incomplete || !bodies_complete,
            ));
            result.saved += new_messages.len();
            continue;
        }

        // A message that was downloaded but not written is as absent from the
        // store as one that was never fetched, so it counts against this
        // target's coverage too.
        //
        // The failed UIDs are collected rather than reduced to a flag, because
        // the pass owes the next one a mark below them (#0074): a flag lives
        // for this pass only, and the next pass would stand on a floor above
        // the message this one downloaded and dropped. `note_ingest_failure`
        // is what keeps that from being permanent for a message the store
        // rejects every time; a UID it has given up on is left out of `unmet`,
        // so it neither lowers the mark nor reports the pass short.
        //
        // One poisoned message never stops the batch: every failure `continue`s
        // to the next message, so the rest of the window is ingested normally
        // and only the prune is held back.
        // The rebind gate (#0112). Ingest may move an existing row onto the UID
        // it is writing only when the server cannot still be holding the UID
        // that row is parked on; otherwise the two are separate server-side
        // copies of one message and each owes a row of its own.
        //
        // Two kinds of pass cannot supply a listing that decides it, and both
        // degrade to the unconditional rebind ingest always did. A UIDVALIDITY
        // reset renumbered the mailbox, so a listed UID says nothing about
        // which message wears it and a row whose old number was recycled must
        // still be allowed to follow its message (#0038). A short enumeration
        // listed fewer UIDs than the mailbox announced under EXISTS, a listing
        // already untrusted for pruning and no more trustworthy here.
        //
        // What survives the degradation is the set growing as the pass ingests:
        // a row this pass has already put on a UID is not moved off it again,
        // which is what keeps N copies of one Message-ID from collapsing back
        // onto the lowest-id row.
        let mut server_uids: std::collections::HashSet<i64> =
            if fetched.uidvalidity_reset || !fetched.enumeration_complete {
                std::collections::HashSet::new()
            } else {
                fetched.listed.iter().map(|&uid| uid as i64).collect()
            };

        let mut unmet: Vec<u32> = Vec::new();
        // #0115: a UID the store has given up on drops out of `unmet`, which is
        // exactly what makes the pass look complete while the message still has
        // no row. The server keeps listing it, so every later pass downloads it
        // again under the same fingerprint: a permanent repeat over a state
        // #0074 declares expected. The flag is what keeps the detector off it.
        let mut gave_up = false;
        let mut note_failure = |uid: u32, error: &str| {
            if note_ingest_failure(store, account, target.role.as_str(), &target.server_name, uid, error)
            {
                unmet.push(uid);
            } else {
                gave_up = true;
            }
        };
        for message in &new_messages {
            let Some(mut email) = parse_rfc822_to_fetched_email(&message.raw) else {
                warn!(
                    "Skipping UID {} in '{}': the message did not parse",
                    message.uid, target.server_name
                );
                note_failure(message.uid, "the message did not parse");
                continue;
            };
            email.flags = message.flags;

            let outcome = ingest::ingest_message_with_policy(
                store,
                blobs,
                &IngestInput {
                    account,
                    mailbox: target.role.as_str(),
                    uid: message.uid as i64,
                    email: &email,
                    raw: Some(&message.raw),
                },
                &RebindPolicy::UnlessListed(&server_uids),
            );
            match outcome {
                Ok(outcome) => {
                    // Whatever row now holds this UID is off limits to the rest
                    // of the pass: see the gate's comment above.
                    server_uids.insert(message.uid as i64);
                    ingest::clear_ingest_failure(
                        store,
                        account,
                        target.role.as_str(),
                        message.uid as i64,
                    );
                    if outcome.inserted {
                        result.saved += 1;
                    }
                    if outcome.uid_rebound {
                        result.uid_rebound += 1;
                    }
                    if outcome.inserted && target.role.is_inbox() {
                        result.new_inbox_mail.push(crate::notify::NewMailMeta::new(
                            &email.from,
                            &email.subject,
                        ));
                    }
                    result.fresh_observations.push(FreshObservation {
                        role: target.role.clone(),
                        from: email.from.clone(),
                        to: email.to.clone(),
                        cc: email.cc.clone(),
                        date: email.date.clone(),
                    });
                }
                Err(e) => {
                    warn!(
                        "Failed to ingest UID {} from '{}': {:#}",
                        message.uid, target.server_name, e
                    );
                    note_failure(message.uid, &format!("{e:#}"));
                }
            }
        }

        // The degradation above is granted to the pass that detects the reset,
        // and until #0117 it died with it: the cursor records the new
        // UIDVALIDITY at the end of this pass, so no later pass reports a
        // reset, while the refetch was capped at the window (100 UIDs on a
        // quick tick, 50 on `mp sync`). Every row below that window kept its
        // old-validity UID into passes that trust their listing, where the gate
        // declined its rebind and the message was inserted a second time.
        //
        // The severe half is not the duplicate: the stale row keeps the
        // recycled UID in the skip list, so the message the server holds there
        // is never downloaded, and the server still lists that UID so no prune
        // clears the row either. Nothing converged it, a full sync included.
        //
        // So the pass says what it verified before it leaves: every UID it
        // ingested is a row the server vouched for, and every other listed UID
        // is a claim made under a numbering that no longer exists. The rows
        // holding those claims are unbound, which frees the UID for the message
        // that now wears it and leaves the row rebindable, so its own message
        // takes it back when a pass downloads it.
        //
        // That pass is a full sync, not the next tick. The fetch window is
        // positional (`listed.iter().rev().take(n)`), so it is the top of the
        // mailbox, and the rows unbound here are below it by construction: a
        // freed UID is not a requested one. What the unbind buys is that a
        // pass covering the whole listing is no longer blocked by a skip-list
        // entry, where before it was. Until such a pass runs, an unbound row
        // holds no server UID, so the prune and the flag pass pass over it.
        //
        // Rows on UIDs the server does not list are left alone: they block
        // nothing, and the prune is the path that answers for them.
        if fetched.uidvalidity_reset {
            let unverified: Vec<u32> = fetched
                .listed
                .iter()
                .copied()
                .filter(|&uid| !server_uids.contains(&(uid as i64)))
                .collect();
            ingest::unbind_rows_on_uids(store, account, target.role.as_str(), &unverified);
        }

        coverage.push((
            fetched.enumeration_complete,
            fetched.download_incomplete || !unmet.is_empty() || !bodies_complete,
        ));
        let pending_arrival_mark = mark_below_unmet(pending_arrival_mark, &unmet);

        // The IMAP server states the whole flag set, so it is truth for all
        // three bits of the second axis (#TKT-0051), not just for `\Seen`.
        result.flags_updated += ingest::apply_flags(
            store,
            account,
            target.role.as_str(),
            fetched
                .known_flags
                .into_iter()
                .map(|(uid, flags)| (uid as i64, flags)),
        );

        // The other half of the same diff: the UIDs the store holds for this
        // mailbox that the server did not list. Held back until every target
        // has been ingested (see the second pass below).
        if !fetched.vanished.is_empty() {
            prunes.push((target.role.clone(), fetched.vanished));
        }

        // #0115: a pass that saw the whole mailbox, downloaded everything it
        // owed and wrote it all has, by construction, nothing left to download
        // next time. When the next such pass downloads the same UIDs again,
        // something upstream is refusing to converge (#0112 was the instance
        // that motivated this), and the streak says so out loud.
        //
        // Every other shape of pass is silent rather than reset: a truncated
        // or short pass is *expected* to leave work behind, so it neither
        // proves nor disproves convergence and leaves the row as it found it.
        // A pass that gave up on a message (#0074) is the same shape: it wrote
        // less than it downloaded, and the give-up is what makes that permanent
        // rather than a bug to report.
        let qualifies = fetched.enumeration_complete
            && !fetched.download_incomplete
            && bodies_complete
            && !fetched.uidvalidity_reset
            && unmet.is_empty()
            && !gave_up;
        if qualifies {
            let uids: Vec<u32> = new_messages.iter().map(|m| m.uid).collect();
            if note_download_fingerprint(store, account, target, &uids).is_some() {
                result.non_converging.push(target.server_name.clone());
            }
        }

        let highest_uid = new_messages.iter().map(|m| m.uid as i64).max();
        ingest::record_mailbox_cursor(
            store,
            account,
            target.role.as_str(),
            &MailboxCursor {
                uidvalidity: state.uid_validity.map(|v| v as i64),
                last_uid: highest_uid.or_else(|| state.uid_next.map(|n| n as i64 - 1)),
                uidnext: state.uid_next.map(|v| v as i64),
                exists: Some(state.exists as i64),
                // `None` here means "this pass has nothing to say about the
                // modseq", not "clear it": the UPSERT COALESCEs, so a
                // full-window pass leaves a CONDSTORE pass's resume point
                // alone (#0041).
                highest_modseq: fetched.highest_modseq.filter(|_| bodies_complete),
                deltalink: None,
                // What this pass owes the next one: the mark below which the
                // gate must stay shut because an arrival the server lists is
                // still not in the store. Written even when it is None, which
                // is how a pass that caught up reopens the gate (#0072).
                arrival_mark: pending_arrival_mark.map(|m| m as i64),
            },
        )?;
    }

    span.mark("ingest");

    // Second pass: every prune, after every target has been ingested.
    //
    // Targets are synced in order (inbox, archive, sent), so pruning inside
    // the loop deletes the inbox row of a message archived in another client
    // *before* the archive pass ingests it: a window in which the store holds
    // no row for that message at all, its blobs drop to refcount zero and are
    // unlinked, and a failed archive fetch (the `continue` above) loses it
    // locally until a later sync. Applying the prunes here means the
    // destination row already exists when the source row goes.
    //
    // Whether they run at all is the coverage gate (#0072): one mailbox that
    // came back short invalidates every target's diff, because the argument
    // that lets an inbox row go is that another target ingested the copy the
    // message moved to.
    if ingest::pass_may_prune(&coverage) {
        let now = crate::outbox::unix_now();
        for (role, vanished) in &prunes {
            // The age guard is the other half: a row this client has just
            // written locally (a Sent copy the server has not filed yet) is in
            // every vanished set until the server's own copy shows up.
            let vanished: Vec<i64> = vanished.iter().map(|&uid| uid as i64).collect();
            let prunable = ingest::prunable_uids(store, account, role.as_str(), &vanished, now);
            result.pruned +=
                ingest::prune_vanished(store, blobs, account, role.as_str(), &prunable);
        }
    } else {
        result.prunes_deferred = prunes.iter().map(|(_, v)| v.len()).sum();
        if result.prunes_deferred > 0 {
            info!(
                "IMAP sync: {} pending prune(s) deferred; this pass did not see every message",
                result.prunes_deferred,
            );
        }
    }
    span.mark("prune");

    Ok(result)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::sync::{FetchedRaw, MailboxState};
    use crate::types::MessageFlags;

    // -----------------------------------------------------------------------
    // The fake backend (#0059)
    // -----------------------------------------------------------------------

    /// A [`SyncBackend`] that answers from a script instead of a server.
    ///
    /// One entry per pass per target, keyed by server name, popped in order, so
    /// a test can say "this is what the server hands back on pass 1, this on
    /// pass 2" and drive the real engine over both. A target with nothing left
    /// scripted gets an error result, which is the engine's "this mailbox did
    /// not sync at all" path.
    #[derive(Default)]
    struct FakeBackend {
        passes: HashMap<String, Vec<Result<MailboxFetch>>>,
        /// Every `(target, limit, skip-list size)` the engine asked for, in
        /// call order: what the engine handed the transport.
        seen: Vec<(String, usize, usize)>,
        /// Opt-in fidelity: drop the scripted messages whose UID the store's
        /// skip list already holds, resolved against the scripted UIDVALIDITY
        /// exactly as [`crate::ingest::KnownUids::resolve`] does it.
        ///
        /// That is what the real backend's pass 2 does
        /// ([`crate::imap_client::fetch_new_raw_on_session`]): it downloads a
        /// UID only when the store does not hold it. Off by default, because
        /// the scripts here mostly say "this is what came back" rather than
        /// "this is what the mailbox holds", and a test that wants to state
        /// what a *stale skip-list entry* costs has to be handed the mailbox
        /// instead (#0117).
        honour_skip_list: bool,
    }

    impl FakeBackend {
        fn script(&mut self, server_name: &str, fetches: Vec<Result<MailboxFetch>>) {
            self.passes.insert(server_name.to_string(), fetches);
        }
    }

    impl SyncBackend for FakeBackend {
        async fn fetch_targets(
            &mut self,
            targets: &[SyncTarget],
            limit: usize,
            knowns: Vec<crate::ingest::KnownUids>,
        ) -> Vec<Result<MailboxFetch>> {
            let mut out = Vec::with_capacity(targets.len());
            for (target, known) in targets.iter().zip(knowns) {
                self.seen.push((target.server_name.clone(), limit, known.uids.len()));
                let mut next = self
                    .passes
                    .get_mut(&target.server_name)
                    .and_then(|queue| {
                        if queue.is_empty() {
                            None
                        } else {
                            Some(queue.remove(0))
                        }
                    })
                    .unwrap_or_else(|| Err(anyhow::anyhow!("nothing scripted")));
                if self.honour_skip_list {
                    if let Ok(fetch) = next.as_mut() {
                        let (skip, _) = known.resolve(fetch.state.uid_validity);
                        fetch.messages.retain(|m| !skip.contains(&(m.uid as i64)));
                    }
                }
                out.push(next);
            }
            out
        }
    }

    fn raw(name: &str) -> Vec<u8> {
        format!(
            "From: a@example.com\r\nTo: b@example.com\r\nSubject: {name}\r\n\
             Message-ID: <{name}@example.com>\r\nDate: Thu, 7 Aug 2025 10:00:00 +0000\r\n\r\n\
             {name} body\r\n"
        )
        .into_bytes()
    }

    /// Bytes the ingest path refuses: a header block that opens with a
    /// continuation line, which is the one shape `mailparse` (and so
    /// [`parse_rfc822_to_fetched_email`]) rejects outright. It stands in for
    /// every "downloaded and not written" message in these tests.
    fn unparsable() -> Vec<u8> {
        b" not a header\r\nFrom: a@example.com\r\n\r\nbody\r\n".to_vec()
    }

    /// A fetch that saw the whole mailbox and downloaded everything it owed:
    /// the shape that opens the prune gate.
    ///
    /// `listed` is derived from the UIDs the caller scripts rather than left
    /// empty, because an empty listing is the #0112 gate's degradation path:
    /// defaulting to it would take every test here down the unconditional
    /// rebind and leave the gate itself unexercised. A test that wants the
    /// degradation says so by clearing the field, and
    /// `an_empty_listing_falls_back_to_the_unconditional_rebind` is where it is
    /// asserted on purpose.
    fn fetch(messages: Vec<(u32, Vec<u8>)>) -> MailboxFetch {
        let listed: Vec<u32> = {
            let mut uids: Vec<u32> = messages.iter().map(|(uid, _)| *uid).collect();
            uids.sort_unstable();
            uids
        };
        MailboxFetch {
            messages: messages
                .into_iter()
                .map(|(uid, raw)| FetchedRaw { uid, raw, flags: MessageFlags::default() })
                .collect(),
            skipped: 0,
            known_flags: Vec::new(),
            state: MailboxState { uid_validity: Some(7), uid_next: Some(200), exists: 2 },
            vanished: Vec::new(),
            listed,
            uidvalidity_reset: false,
            enumeration_complete: true,
            download_incomplete: false,
            pending_arrival_mark: None,
            // #0113: the scripted pass asked for every new UID it was given.
            // A test that wants a deadline-truncated pass clears this.
            bodies_complete: true,
            // #0041 added this field; the fake backend is a non-CONDSTORE
            // server, which is what every existing engine test assumed and
            // still asserts.
            highest_modseq: None,
        }
    }

    fn targets() -> Vec<SyncTarget> {
        vec![
            SyncTarget { role: MailboxRole::Inbox, server_name: "INBOX".into() },
            SyncTarget { role: MailboxRole::Archive, server_name: "Archive".into() },
        ]
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        store: Store,
        blobs: BlobStore,
    }

    impl Fixture {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let store = Store::open(tmp.path().join("store.sqlite3")).unwrap();
            let blobs = BlobStore::new(tmp.path().join("blobs"));
            Self { _tmp: tmp, store, blobs }
        }

        fn run(&self, backend: &mut FakeBackend, targets: &[SyncTarget]) -> SyncResult {
            self.run_with(backend, targets, usize::MAX, false)
        }

        fn run_with(
            &self,
            backend: &mut FakeBackend,
            targets: &[SyncTarget],
            limit: usize,
            dry_run: bool,
        ) -> SyncResult {
            let run = SyncRun {
                store: &self.store,
                blobs: &self.blobs,
                account: "acct",
                targets,
                limit,
                dry_run,
            };
            let mut span = TimingSpan::new("test");
            futures::executor::block_on(run_sync(backend, &run, &mut span)).unwrap()
        }

        fn rows(&self, mailbox: &str) -> Vec<i64> {
            let conn = self.store.conn();
            let mut stmt = conn
                .prepare(
                    "SELECT uid FROM messages WHERE account = 'acct' AND mailbox = ?1 \
                     ORDER BY uid",
                )
                .unwrap();
            let out = stmt
                .query_map([mailbox], |row| row.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            out
        }

        /// `(row id, uid)` for the mailbox, lowest UID first: what `rows`
        /// cannot say, which is whether a message kept the row it had or was
        /// written into a new one.
        fn row_ids(&self, mailbox: &str) -> Vec<(i64, i64)> {
            let conn = self.store.conn();
            let mut stmt = conn
                .prepare(
                    "SELECT id, uid FROM messages WHERE account = 'acct' AND mailbox = ?1 \
                     ORDER BY uid",
                )
                .unwrap();
            let out = stmt
                .query_map([mailbox], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            out
        }

        /// `(row id, uid)` of every row in the mailbox carrying `message_id`,
        /// which is what says whether a message ended on one row and which row
        /// that is: `rows` alone cannot tell a message that followed its row
        /// from one written into a fresh one beside it.
        fn rows_for(&self, mailbox: &str, message_id: &str) -> Vec<(i64, i64)> {
            let conn = self.store.conn();
            let mut stmt = conn
                .prepare(
                    "SELECT id, uid FROM messages WHERE account = 'acct' AND mailbox = ?1 \
                     AND message_id = ?2 ORDER BY uid",
                )
                .unwrap();
            let out = stmt
                .query_map((mailbox, message_id), |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            out
        }

        fn modseq(&self, mailbox: &str) -> Option<i64> {
            ingest::load_mailbox_cursor(&self.store, "acct", mailbox)
                .unwrap()
                .and_then(|c| c.highest_modseq)
        }

        fn cursor_mark(&self, mailbox: &str) -> Option<u32> {
            ingest::known_uids_with_cursor(&self.store, "acct", mailbox).unwrap().arrival_mark
        }

        /// The persisted `(hash, count, streak)` of the #0115 detector, or
        /// `None` when the mailbox has no marker row.
        fn streak(&self, mailbox: &str) -> Option<(u64, usize, u32)> {
            schema::get_meta(self.store.conn(), &nonconverging_key(mailbox))
                .unwrap()
                .as_deref()
                .and_then(parse_streak_row)
        }
    }

    // -----------------------------------------------------------------------
    // Engine tests: the loop itself, driven by the fake backend
    // -----------------------------------------------------------------------

    /// #0115, the pure half: only an identical fingerprint continues a streak,
    /// and the log line is throttled to 2, 3, then every tenth.
    #[test]
    fn a_streak_continues_only_on_an_identical_fingerprint() {
        let first = fingerprint(&[102, 101]);
        assert_eq!(first, fingerprint(&[101, 102]), "the UID set is order-independent");
        assert_ne!(first, fingerprint(&[101, 103]));

        assert_eq!(advance_streak(None, first), (first.0, first.1, 1));
        assert_eq!(advance_streak(Some((first.0, first.1, 1)), first), (first.0, first.1, 2));
        assert_eq!(advance_streak(Some((first.0, first.1, 9)), first), (first.0, first.1, 10));

        let other = fingerprint(&[101, 103]);
        assert_eq!(
            advance_streak(Some((first.0, first.1, 7)), other),
            (other.0, other.1, 1),
            "a different set starts over"
        );
        // A count that disagrees is a new fingerprint even if the hash did not.
        assert_eq!(advance_streak(Some((first.0, first.1 + 1, 4)), first), (first.0, first.1, 1));

        let loud: Vec<u32> = (1..=32).filter(|&s| streak_is_loud(s)).collect();
        assert_eq!(loud, vec![2, 3, 10, 20, 30]);

        assert_eq!(parse_streak_row("7:2:3"), Some((7, 2, 3)));
        assert_eq!(parse_streak_row("7:2"), None, "a malformed row starts the streak over");
        assert_eq!(parse_streak_row("7:2:3:4"), None);
    }

    /// #0115: two complete passes that download the same UIDs are the #0112
    /// shape, and the pass that sees the repeat says so. A third repeat keeps
    /// saying it; a pass with a different set is the fetch converging again.
    #[test]
    fn a_pass_that_downloads_the_same_uids_again_reports_a_fetch_that_is_not_converging() {
        let fx = Fixture::new();
        let targets = vec![SyncTarget { role: MailboxRole::Inbox, server_name: "INBOX".into() }];
        let mut backend = FakeBackend::default();
        let same = || Ok(fetch(vec![(101, raw("one")), (102, raw("two"))]));
        backend.script(
            "INBOX",
            vec![same(), same(), same(), Ok(fetch(vec![(103, raw("three"))]))],
        );

        let first = fx.run(&mut backend, &targets);
        assert!(first.non_converging.is_empty(), "one download of a UID set proves nothing");
        assert_eq!(fx.streak("inbox").map(|s| s.2), Some(1));

        let second = fx.run(&mut backend, &targets);
        assert_eq!(second.non_converging, vec!["INBOX".to_string()]);
        assert_eq!(fx.streak("inbox").map(|s| s.2), Some(2));

        let third = fx.run(&mut backend, &targets);
        assert_eq!(third.non_converging, vec!["INBOX".to_string()], "and keeps saying it");
        assert_eq!(fx.streak("inbox").map(|s| s.2), Some(3));

        let moved_on = fx.run(&mut backend, &targets);
        assert!(moved_on.non_converging.is_empty(), "a different set is a fetch making progress");
        assert_eq!(fx.streak("inbox").map(|s| s.2), Some(1));
    }

    /// #0115: a pass that is short by design is expected to leave work behind,
    /// so it neither counts as a repeat nor clears the streak the passes before
    /// it built up.
    #[test]
    fn a_short_repeat_pass_neither_counts_nor_resets_the_streak() {
        let fx = Fixture::new();
        let targets = vec![SyncTarget { role: MailboxRole::Inbox, server_name: "INBOX".into() }];
        let mut backend = FakeBackend::default();
        let same = || fetch(vec![(101, raw("one")), (102, raw("two"))]);
        let truncated = || {
            let mut f = same();
            f.bodies_complete = false;
            f.download_incomplete = true;
            Ok(f)
        };
        backend.script("INBOX", vec![Ok(same()), truncated(), truncated()]);

        fx.run(&mut backend, &targets);
        assert_eq!(fx.streak("inbox").map(|s| s.2), Some(1));

        let cut = fx.run(&mut backend, &targets);
        assert!(cut.non_converging.is_empty(), "a truncated pass may repeat itself");
        assert_eq!(fx.streak("inbox").map(|s| s.2), Some(1), "and leaves the marker alone");

        let cut_again = fx.run(&mut backend, &targets);
        assert!(cut_again.non_converging.is_empty());
        assert_eq!(fx.streak("inbox").map(|s| s.2), Some(1));
    }

    /// #0115: a UIDVALIDITY reset renumbered the mailbox, so the fingerprint
    /// the marker holds describes UIDs that no longer mean anything.
    #[test]
    fn a_uidvalidity_reset_clears_the_convergence_marker() {
        let fx = Fixture::new();
        let targets = vec![SyncTarget { role: MailboxRole::Inbox, server_name: "INBOX".into() }];
        let mut backend = FakeBackend::default();
        let same = || Ok(fetch(vec![(101, raw("one")), (102, raw("two"))]));
        let mut reset = fetch(vec![(1, raw("one")), (2, raw("two"))]);
        reset.uidvalidity_reset = true;
        reset.state.uid_validity = Some(8);
        backend.script("INBOX", vec![same(), same(), Ok(reset)]);

        fx.run(&mut backend, &targets);
        let repeat = fx.run(&mut backend, &targets);
        assert_eq!(repeat.non_converging, vec!["INBOX".to_string()]);

        let after = fx.run(&mut backend, &targets);
        assert!(after.non_converging.is_empty());
        assert_eq!(fx.streak("inbox"), None, "the marker is dropped with the modseq");
    }

    /// #0115: a pass with nothing new is the fetch converging, which is what
    /// the detector is watching for, so it drops the marker.
    #[test]
    fn a_pass_with_nothing_new_clears_the_convergence_marker() {
        let fx = Fixture::new();
        let targets = vec![SyncTarget { role: MailboxRole::Inbox, server_name: "INBOX".into() }];
        let mut backend = FakeBackend::default();
        let same = || Ok(fetch(vec![(101, raw("one")), (102, raw("two"))]));
        backend.script("INBOX", vec![same(), same(), Ok(fetch(vec![]))]);

        fx.run(&mut backend, &targets);
        assert_eq!(fx.run(&mut backend, &targets).non_converging, vec!["INBOX".to_string()]);

        let quiet = fx.run(&mut backend, &targets);
        assert!(quiet.non_converging.is_empty());
        assert_eq!(fx.streak("inbox"), None);
    }

    /// #0115 review: the give-up (#0074) must not read as a bug. A message the
    /// store rejects every pass is dropped from `unmet` once it has burned
    /// [`ingest::MAX_INGEST_ATTEMPTS`], so from then on every pass looks
    /// complete while still downloading that UID under an unchanging
    /// fingerprint. Without the `gave_up` gate the detector would fire forever
    /// on a state #0074 declares expected.
    #[test]
    fn a_message_the_store_gave_up_on_is_not_a_fetch_that_fails_to_converge() {
        let fx = Fixture::new();
        let targets = vec![SyncTarget { role: MailboxRole::Inbox, server_name: "INBOX".into() }];
        let mut backend = FakeBackend::default();
        let poisoned = || Ok(fetch(vec![(105, unparsable())]));
        let passes = ingest::MAX_INGEST_ATTEMPTS + 2;
        backend.script("INBOX", (0..passes).map(|_| poisoned()).collect());

        for pass in 1..=passes {
            let result = fx.run(&mut backend, &targets);
            assert!(
                result.non_converging.is_empty(),
                "pass {pass} downloaded a message it cannot write, which is not a repeat"
            );
            assert_eq!(fx.streak("inbox"), None, "pass {pass} leaves no marker behind");
        }

        assert!(
            ingest::ingest_failure_attempts(&fx.store, "acct", "inbox", 105)
                >= ingest::MAX_INGEST_ATTEMPTS,
            "the store did give up, so the silence is the gave-up path and not a retry"
        );
        assert!(fx.rows("inbox").is_empty(), "and the message still has no row");
    }

    /// The baseline: what the backend hands back is ingested, counted and
    /// cursored, and the next pass's skip list is what the first pass wrote.
    #[test]
    fn the_engine_ingests_what_the_backend_returns_and_advances_the_cursor() {
        let fx = Fixture::new();
        let targets = targets();
        let mut backend = FakeBackend::default();
        backend.script("INBOX", vec![Ok(fetch(vec![(101, raw("one")), (102, raw("two"))]))]);
        backend.script("Archive", vec![Ok(fetch(vec![(55, raw("old"))]))]);

        let result = fx.run(&mut backend, &targets);

        assert_eq!(result.saved, 3);
        assert_eq!(result.new_inbox_mail.len(), 2, "only inbox arrivals notify");
        assert_eq!(result.fresh_observations.len(), 3, "every ingest feeds the contacts hook");
        assert_eq!(fx.rows("inbox"), vec![101, 102]);
        assert_eq!(fx.rows("archive"), vec![55]);
        // The cursor is the highest UID this pass ingested, and the next pass
        // is handed exactly the UIDs the store now holds.
        let known = ingest::known_uids_with_cursor(&fx.store, "acct", "inbox").unwrap();
        assert_eq!(known.prior_high_water, Some(102));
        assert_eq!(known.uidvalidity, Some(7));

        backend.script("INBOX", vec![Ok(fetch(vec![]))]);
        backend.script("Archive", vec![Ok(fetch(vec![]))]);
        fx.run(&mut backend, &targets);
        let asked = &backend.seen[2..];
        assert_eq!(
            asked,
            &[("INBOX".to_string(), usize::MAX, 2), ("Archive".to_string(), usize::MAX, 1)],
            "pass 2 hands the backend the skip list pass 1 wrote, in target order"
        );
    }

    /// #0074, through the real loop this time: a message the engine downloads
    /// and cannot write pulls the persisted arrival mark under itself, holds
    /// this pass's prune back, and is retried on the next pass, which writes it
    /// exactly once and reopens the gate.
    ///
    /// The pre-#0059 version of this test re-walked `note_ingest_failure`,
    /// `mark_below_unmet` and `record_mailbox_cursor` by hand and could only
    /// claim that the loop called them in that order; here the loop does.
    #[test]
    fn an_unwritable_message_holds_the_mark_down_and_the_retry_writes_it_once() {
        let fx = Fixture::new();
        let targets = vec![SyncTarget { role: MailboxRole::Inbox, server_name: "INBOX".into() }];
        let mut backend = FakeBackend::default();

        // Pass 0 seeds the row a later prune will delete, so `pruned` is
        // observable. Pass 1: UID 105 does not parse and cannot be written,
        // UID 104 lands beside it, and UID 90 is offered as vanished to show
        // the write failure holds the prune back too. Pass 2 hands 105 back
        // (it is not in the store, so a real backend re-downloads it) parsable.
        let mut poisoned = fetch(vec![(104, raw("below")), (105, unparsable())]);
        poisoned.vanished = vec![90];
        let mut good = fetch(vec![(105, raw("retried"))]);
        good.vanished = vec![90];
        backend.script(
            "INBOX",
            vec![Ok(fetch(vec![(90, raw("doomed"))])), Ok(poisoned), Ok(good)],
        );

        let seed = fx.run(&mut backend, &targets);
        assert_eq!(seed.saved, 1);

        let result = fx.run(&mut backend, &targets);

        assert_eq!(result.saved, 1, "the poisoned message does not stop the one beside it");
        assert_eq!(fx.rows("inbox"), vec![90, 104], "and 105 is simply not there");
        assert_eq!(
            fx.cursor_mark("inbox"),
            Some(104),
            "the mark sits below the message that was not written"
        );
        assert_eq!(result.pruned, 0, "and the same failure suspends this pass's prune");
        assert_eq!(result.prunes_deferred, 1);
        assert_eq!(ingest::ingest_failure_attempts(&fx.store, "acct", "inbox", 105), 1);

        let result = fx.run(&mut backend, &targets);

        assert_eq!(result.saved, 1);
        assert_eq!(fx.cursor_mark("inbox"), None, "a pass that wrote what it owed reopens the gate");
        assert_eq!(ingest::ingest_failure_attempts(&fx.store, "acct", "inbox", 105), 0);
        assert_eq!(result.pruned, 1, "and the reopened gate applies the prune it deferred");
        assert_eq!(fx.rows("inbox"), vec![104, 105], "written once, and the vanished row is gone");
    }

    /// #0113: a body pass that stopped at its deadline is a short pass, and
    /// the engine treats it as one all the way through.
    ///
    /// The three consequences are the whole reason the truncation is safe. The
    /// prune is suspended, because the copy that would justify a deletion may
    /// be exactly the body this pass did not ask for. No modseq is recorded,
    /// because a `CHANGEDSINCE` resuming from it would skip every later flag
    /// change on the messages left behind. And the pass is counted, so the
    /// status line can say the tick was cut rather than reporting it clean.
    ///
    /// The fourth is the point of the ticket: the next pass picks up the
    /// backlog from the same cursor and converges, no full sync required.
    #[test]
    fn a_pass_cut_by_the_body_deadline_defers_the_prune_records_no_modseq_and_resumes() {
        let fx = Fixture::new();
        let targets = vec![SyncTarget { role: MailboxRole::Inbox, server_name: "INBOX".into() }];
        let mut backend = FakeBackend::default();

        // Pass 0 seeds the row the prune will want to delete, so `pruned` is
        // observable. Pass 1 is the truncated one: the server lists 90, 91 and
        // 92, the deadline stopped the body fetch after the newest (92), and 91
        // was never asked for. Pass 2 downloads the backlog and completes.
        let mut truncated = fetch(vec![(92, raw("newest"))]);
        truncated.bodies_complete = false;
        truncated.download_incomplete = true;
        truncated.listed = vec![90, 91, 92];
        truncated.vanished = vec![90];
        truncated.highest_modseq = Some(4_000);
        let mut rest = fetch(vec![(91, raw("older"))]);
        rest.listed = vec![91, 92];
        rest.vanished = vec![90];
        rest.highest_modseq = Some(4_100);
        backend.script(
            "INBOX",
            vec![Ok(fetch(vec![(90, raw("doomed"))])), Ok(truncated), Ok(rest)],
        );

        let seed = fx.run(&mut backend, &targets);
        assert_eq!(seed.saved, 1);

        let cut = fx.run(&mut backend, &targets);

        assert_eq!(cut.saved, 1, "a stopped pass still ingests every body it did collect");
        assert_eq!(cut.bodies_truncated, 1, "and says which mailbox was cut");
        assert_eq!(cut.pruned, 0, "a pass that skipped a body may not delete a row");
        assert_eq!(cut.prunes_deferred, 1);
        assert_eq!(fx.modseq("inbox"), None, "and vouches for no flag it did not look at");
        assert_eq!(fx.rows("inbox"), vec![90, 92]);

        let done = fx.run(&mut backend, &targets);

        assert_eq!(done.saved, 1, "the backlog resumes on the next pass, from the same cursor");
        assert_eq!(done.bodies_truncated, 0);
        assert_eq!(done.pruned, 1, "which reopens the gate and applies the prune it held");
        assert_eq!(fx.rows("inbox"), vec![91, 92]);
        assert_eq!(fx.modseq("inbox"), Some(4_100), "a complete pass records its resume point");
    }

    /// #0074: the mark may not become a deadlock. A message the store rejects
    /// every pass is given up on after [`ingest::MAX_INGEST_ATTEMPTS`], and the
    /// pass after that stops reporting itself short, which is what stops one
    /// unwritable message from suspending the prune for the account for good.
    #[test]
    fn a_permanently_unwritable_message_stops_holding_the_prune_after_three_passes() {
        let fx = Fixture::new();
        let targets = vec![SyncTarget { role: MailboxRole::Inbox, server_name: "INBOX".into() }];
        let mut backend = FakeBackend::default();
        let poisoned = || {
            let mut f = fetch(vec![(105, unparsable())]);
            f.vanished = vec![90];
            Ok(f)
        };
        backend.script("INBOX", vec![poisoned(), poisoned(), poisoned(), poisoned()]);

        for pass in 1..ingest::MAX_INGEST_ATTEMPTS {
            let result = fx.run(&mut backend, &targets);
            assert_eq!(fx.cursor_mark("inbox"), Some(104), "pass {pass} still owes the message");
            assert_eq!(result.pruned, 0, "pass {pass} keeps the prune suspended");
        }

        // The last attempt is the give-up: the UID drops out of `unmet`, so it
        // neither lowers the mark nor reports the pass short from here on.
        let result = fx.run(&mut backend, &targets);
        assert_eq!(fx.cursor_mark("inbox"), None, "a given-up UID leaves no mark behind");
        assert_eq!(result.prunes_deferred, 0, "and no longer reports the pass short");
        assert_eq!(ingest::ingest_failure_attempts(&fx.store, "acct", "inbox", 105), 3);
    }

    /// #0074 review, through the loop: a UIDVALIDITY reset clears the mailbox's
    /// failure counts, because they are keyed by UID and the server has just
    /// renumbered them. The message that lands on the recycled UID gets its
    /// full three attempts rather than inheriting a give-up.
    #[test]
    fn a_uidvalidity_reset_clears_the_mailboxs_failure_counts() {
        let fx = Fixture::new();
        let targets = vec![SyncTarget { role: MailboxRole::Inbox, server_name: "INBOX".into() }];
        let mut backend = FakeBackend::default();
        let poisoned = || Ok(fetch(vec![(105, unparsable())]));
        let mut after_reset = fetch(vec![(105, unparsable())]);
        after_reset.uidvalidity_reset = true;
        backend.script("INBOX", vec![poisoned(), poisoned(), Ok(after_reset), poisoned()]);

        fx.run(&mut backend, &targets);
        fx.run(&mut backend, &targets);
        assert_eq!(ingest::ingest_failure_attempts(&fx.store, "acct", "inbox", 105), 2);

        let result = fx.run(&mut backend, &targets);
        assert_eq!(result.uidvalidity_resets, 1);
        assert_eq!(
            ingest::ingest_failure_attempts(&fx.store, "acct", "inbox", 105),
            1,
            "the reset wiped the count and this pass's own failure is the first again"
        );
        fx.run(&mut backend, &targets);
        assert_eq!(ingest::ingest_failure_attempts(&fx.store, "acct", "inbox", 105), 2);
        assert_eq!(fx.cursor_mark("inbox"), Some(104), "still retrying, so still owed");
    }

    /// #0072/#0055: prunes run after *every* target is ingested, so a message
    /// archived in another client has its archive row before its inbox row
    /// goes, and never spends a window with no row anywhere.
    #[test]
    fn a_message_moved_between_mailboxes_is_ingested_before_its_old_row_is_pruned() {
        let fx = Fixture::new();
        let targets = targets();
        let mut backend = FakeBackend::default();
        backend.script("INBOX", vec![Ok(fetch(vec![(101, raw("moved"))]))]);
        backend.script("Archive", vec![Ok(fetch(vec![]))]);
        fx.run(&mut backend, &targets);
        assert_eq!(fx.rows("inbox"), vec![101]);

        // Now the server says: gone from INBOX, present in Archive.
        let mut gone = fetch(vec![]);
        gone.vanished = vec![101];
        backend.script("INBOX", vec![Ok(gone)]);
        backend.script("Archive", vec![Ok(fetch(vec![(7, raw("moved"))]))]);
        let result = fx.run(&mut backend, &targets);

        assert_eq!(result.pruned, 1);
        assert!(fx.rows("inbox").is_empty(), "the inbox row goes");
        assert_eq!(fx.rows("archive"), vec![7], "and the archive row is already there");
    }

    /// The coverage gate is account-wide: one target that came back short
    /// suspends every target's prune, because the argument that lets an inbox
    /// row go is that another target ingested the copy it moved to.
    #[test]
    fn a_short_target_defers_every_targets_prune() {
        let fx = Fixture::new();
        let targets = targets();
        let mut backend = FakeBackend::default();
        backend.script("INBOX", vec![Ok(fetch(vec![(101, raw("one"))]))]);
        backend.script("Archive", vec![Ok(fetch(vec![(55, raw("two"))]))]);
        fx.run(&mut backend, &targets);

        let mut inbox_gone = fetch(vec![]);
        inbox_gone.vanished = vec![101];
        backend.script("INBOX", vec![Ok(inbox_gone)]);
        // The archive fetch failed outright, which is the strongest partial
        // pass: the engine keeps going and prunes nothing.
        backend.script("Archive", vec![Err(anyhow::anyhow!("connection reset"))]);
        let result = fx.run(&mut backend, &targets);

        assert_eq!(result.pruned, 0);
        assert_eq!(result.prunes_deferred, 1);
        assert_eq!(fx.rows("inbox"), vec![101], "the row stays until a pass sees everything");
        assert_eq!(fx.rows("archive"), vec![55], "and the failed target is left untouched");
    }

    /// `dry_run` counts and writes nothing: no rows, no cursor, no prune.
    #[test]
    fn a_dry_run_touches_neither_the_store_nor_the_blobs() {
        let fx = Fixture::new();
        let targets = vec![SyncTarget { role: MailboxRole::Inbox, server_name: "INBOX".into() }];
        let mut backend = FakeBackend::default();
        let mut f = fetch(vec![(101, raw("one")), (102, raw("two"))]);
        f.vanished = vec![90];
        backend.script("INBOX", vec![Ok(f)]);

        let result = fx.run_with(&mut backend, &targets, 50, true);

        assert_eq!(result.saved, 2, "it still reports what it would have ingested");
        assert!(fx.rows("inbox").is_empty());
        assert_eq!(result.pruned, 0);
        assert!(result.fresh_observations.is_empty(), "and feeds the contacts hook nothing");
        assert_eq!(backend.seen[0].1, 50, "the limit reaches the transport verbatim");
    }

    /// Flags are the second status axis (#TKT-0051) and arrive only on rows the
    /// store already holds, via `known_flags`.
    #[test]
    fn known_flags_from_the_backend_are_applied_to_rows_the_store_already_holds() {
        let fx = Fixture::new();
        let targets = vec![SyncTarget { role: MailboxRole::Inbox, server_name: "INBOX".into() }];
        let mut backend = FakeBackend::default();
        backend.script("INBOX", vec![Ok(fetch(vec![(101, raw("one"))]))]);
        fx.run(&mut backend, &targets);

        let mut flagged = fetch(vec![]);
        flagged.known_flags = vec![(101, MessageFlags { seen: true, answered: true, ..Default::default() })];
        flagged.skipped = 1;
        backend.script("INBOX", vec![Ok(flagged)]);
        let result = fx.run(&mut backend, &targets);

        assert_eq!(result.flags_updated, 1);
        assert_eq!(result.skipped, 1);
        let flags: String = fx
            .store
            .conn()
            .query_row(
                "SELECT flags FROM messages WHERE account = 'acct' AND mailbox = 'inbox' AND uid = 101",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(flags.contains("\\Seen") && flags.contains("\\Answered"));
    }

    /// #0041, end to end through the engine: the CONDSTORE resume point
    /// survives the passes that know nothing about it, and dies with a
    /// UIDVALIDITY reset.
    ///
    /// This is the loop's half of the carry-forward hazard. The engine writes
    /// whatever the fetch reported, `None` included, and `None` has to mean
    /// "nothing to say" rather than "clear it": a quick sync reports `None` on
    /// every mailbox bigger than its window, so the unconditional write would
    /// have erased the resume point on the very next pass and the delta would
    /// have flapped in and out of use forever, silently.
    #[test]
    fn a_condstore_modseq_survives_the_passes_that_cannot_vouch_for_one() {
        let fx = Fixture::new();
        let targets = vec![SyncTarget { role: MailboxRole::Inbox, server_name: "INBOX".into() }];
        let mut backend = FakeBackend::default();

        // Pass 1 is a full pass over a CONDSTORE server: it records a modseq.
        let mut condstore = fetch(vec![(101, raw("one"))]);
        condstore.highest_modseq = Some(90_060_115_205_545_359);
        // Pass 2 is an ordinary capped pass, which reports no modseq at all.
        let quick = fetch(vec![(102, raw("two"))]);
        // Pass 3 is the renumbering.
        let mut renumbered = fetch(vec![(1, raw("three"))]);
        renumbered.uidvalidity_reset = true;
        backend.script("INBOX", vec![Ok(condstore), Ok(quick), Ok(renumbered)]);

        fx.run(&mut backend, &targets);
        assert_eq!(
            fx.modseq("inbox"),
            Some(90_060_115_205_545_359),
            "the CONDSTORE pass records its resume point"
        );

        fx.run(&mut backend, &targets);
        assert_eq!(
            fx.modseq("inbox"),
            Some(90_060_115_205_545_359),
            "and a pass with nothing to say about it must not erase it"
        );
        // ...and the next fetch is handed it back as its resume point.
        let known = ingest::known_uids_with_cursor(&fx.store, "acct", "inbox").unwrap();
        assert_eq!(known.highest_modseq, Some(90_060_115_205_545_359));

        fx.run(&mut backend, &targets);
        assert_eq!(
            fx.modseq("inbox"),
            None,
            "a UIDVALIDITY reset is the one thing that clears it: the modseq \
             described a mailbox that no longer exists"
        );
        assert!(
            ingest::known_uids_with_cursor(&fx.store, "acct", "inbox")
                .unwrap()
                .highest_modseq
                .is_none(),
            "so the next pass does the full window rather than a delta"
        );
    }

    // -----------------------------------------------------------------------
    // The rebind gate (#0112)
    // -----------------------------------------------------------------------

    /// The reported loop, through the real engine. The server lists three UIDs
    /// carrying one `Message-ID`, which is three messages in that mailbox and
    /// owes three rows. Collapsing them onto one row is what kept the other two
    /// out of the skip list, so every pass reported them new, downloaded them
    /// in full and rebound the row back, forever.
    ///
    /// The second pass is the regression assertion: the skip list the engine
    /// hands the transport holds all three UIDs, which under the bug held one.
    #[test]
    fn n_listed_copies_of_one_message_id_get_n_rows_and_the_next_pass_skips_them_all() {
        let fx = Fixture::new();
        let targets = vec![SyncTarget { role: MailboxRole::Sent, server_name: "Sent Items".into() }];
        let mut backend = FakeBackend::default();
        let copy = || raw("dup");
        backend.script(
            "Sent Items",
            vec![
                Ok(fetch(vec![(6540, copy()), (6542, copy()), (6543, copy())])),
                Ok({
                    // Pass 2: the server lists the same three UIDs and the
                    // store holds them all, so nothing is new.
                    let mut f = fetch(vec![]);
                    f.listed = vec![6540, 6542, 6543];
                    f.skipped = 3;
                    f
                }),
            ],
        );

        let first = fx.run(&mut backend, &targets);

        assert_eq!(first.saved, 3, "three listed copies, three rows");
        assert_eq!(first.uid_rebound, 0, "a listed UID is not a renumbering");
        assert_eq!(fx.rows("sent"), vec![6540, 6542, 6543]);

        let second = fx.run(&mut backend, &targets);

        assert_eq!(second.saved, 0, "an unchanged mailbox has nothing new on the next pass");
        assert_eq!(second.uid_rebound, 0);
        assert_eq!(fx.rows("sent"), vec![6540, 6542, 6543], "and no row's uid moved");
        assert_eq!(
            backend.seen[1].2, 3,
            "the skip list pass 2 is handed holds every copy the server lists"
        );
    }

    /// Finding A, through the engine: two rows parked on the `-id` move
    /// sentinel with one `Message-ID`. The destination pass must rebind both,
    /// not rebind the first and strand the second on a negative UID that
    /// nothing prunes and nothing rebinds again.
    #[test]
    fn both_rows_on_the_move_sentinel_are_rebound_when_the_destination_syncs() {
        let fx = Fixture::new();
        let targets = targets();
        let mut backend = FakeBackend::default();
        let copy = || raw("dup");
        backend.script("INBOX", vec![Ok(fetch(vec![(101, copy()), (102, copy())]))]);
        backend.script("Archive", vec![Ok(fetch(vec![]))]);
        fx.run(&mut backend, &targets);
        assert_eq!(fx.rows("inbox"), vec![101, 102]);

        // The user archives both copies. Each row parks on `uid = -id`.
        let ids: Vec<i64> = {
            let conn = fx.store.conn();
            let mut stmt = conn
                .prepare("SELECT id FROM messages WHERE mailbox = 'inbox' ORDER BY id")
                .unwrap();
            let out = stmt.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
            out
        };
        for id in &ids {
            crate::store::write::move_row(&fx.store, *id, "archive").unwrap();
        }
        assert_eq!(fx.rows("archive"), vec![-ids[1], -ids[0]], "both wait on the sentinel");

        // The server acknowledges: gone from INBOX, two copies in Archive.
        let mut gone = fetch(vec![]);
        gone.vanished = vec![101, 102];
        backend.script("INBOX", vec![Ok(gone)]);
        backend.script("Archive", vec![Ok(fetch(vec![(7, copy()), (8, copy())]))]);
        let result = fx.run(&mut backend, &targets);

        assert_eq!(result.uid_rebound, 2, "both sentinel rows follow their message");
        assert_eq!(result.saved, 0, "and neither copy is invented as a third row");
        assert_eq!(fx.rows("archive"), vec![7, 8]);
        assert_eq!(
            fx.store
                .conn()
                .query_row("SELECT COUNT(*) FROM messages WHERE uid < 0", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0,
            "no row is stranded on a negative UID"
        );
    }

    /// The degradation, stated explicitly (#0112, review finding E and G). A
    /// backend that cannot list, and a pass whose enumeration came back short,
    /// both hand the engine a listing it may not decline a rebind with, so the
    /// rebind is taken unconditionally as it always was. Every other test here
    /// derives `listed` from the UIDs it scripts precisely so it does *not*
    /// come down this path.
    #[test]
    fn an_empty_listing_falls_back_to_the_unconditional_rebind() {
        for short_enumeration in [false, true] {
            let fx = Fixture::new();
            let targets =
                vec![SyncTarget { role: MailboxRole::Sent, server_name: "Sent Items".into() }];
            let mut backend = FakeBackend::default();
            let blind = |uid: u32| {
                let mut f = fetch(vec![(uid, raw("dup"))]);
                if short_enumeration {
                    // The server listed fewer UIDs than it announced under
                    // EXISTS: a listing already untrusted for pruning.
                    f.enumeration_complete = false;
                } else {
                    // A backend with no listing at all, which is what the Graph
                    // path is: its synthetic uid is a content hash that never
                    // renumbers.
                    f.listed = Vec::new();
                }
                Ok(f)
            };
            backend.script("Sent Items", vec![blind(6540), blind(6542)]);

            let first = fx.run(&mut backend, &targets);
            assert_eq!(first.saved, 1);
            let second = fx.run(&mut backend, &targets);

            assert_eq!(second.saved, 0, "short_enumeration={short_enumeration}");
            assert_eq!(second.uid_rebound, 1, "the rebind is taken with no listing to decline it");
            assert_eq!(fx.rows("sent"), vec![6542], "one row, moved onto the newer UID");
        }
    }

    /// A UIDVALIDITY reset renumbers the mailbox, so its listing says nothing
    /// about which message wears which UID and the rebind must be taken even
    /// onto a listed one. What survives the degradation is that the pass does
    /// not rebind twice onto one row: N copies come out of a reset as N rows.
    #[test]
    fn a_reset_rebinds_every_copy_onto_its_own_row() {
        let fx = Fixture::new();
        let targets = vec![SyncTarget { role: MailboxRole::Sent, server_name: "Sent Items".into() }];
        let mut backend = FakeBackend::default();
        let copy = || raw("dup");
        let mut renumbered = fetch(vec![(11, copy()), (12, copy())]);
        renumbered.uidvalidity_reset = true;
        backend.script(
            "Sent Items",
            vec![Ok(fetch(vec![(6540, copy()), (6542, copy())])), Ok(renumbered)],
        );

        fx.run(&mut backend, &targets);
        assert_eq!(fx.rows("sent"), vec![6540, 6542]);

        let result = fx.run(&mut backend, &targets);

        assert_eq!(result.uidvalidity_resets, 1);
        assert_eq!(result.uid_rebound, 2, "each copy follows the row it was on");
        assert_eq!(result.saved, 0, "a renumbering must not duplicate the mailbox");
        assert_eq!(fx.rows("sent"), vec![11, 12]);
    }

    /// The `fetched.uidvalidity_reset` half of the seeding guard, on the shape
    /// that makes it load-bearing: a pass-2 listing that still holds the UIDs
    /// the store's rows are parked on.
    ///
    /// `a_reset_rebinds_every_copy_onto_its_own_row` above cannot see the guard
    /// at all, because the UIDs it renumbers onto (11, 12) do not overlap the
    /// ones the rows sit on (6540, 6542), so membership never discriminates.
    /// The overlap is the realistic reset: a recreated mailbox restarts its
    /// numbering low over a store that still holds those same low numbers, and
    /// here the two messages shift down by one, from 11 and 12 onto 10 and 11.
    ///
    /// Seeded with that listing the gate declines both rebinds, so the pass
    /// inserts a second row for the message on 10, overwrites the row on 11
    /// with the other message's envelope through the identity lookup, and
    /// strands the original row on 12: three rows for a two-message mailbox,
    /// one of them holding a UID the server no longer lists.
    #[test]
    fn a_reset_rebinds_a_row_parked_on_a_uid_its_own_listing_still_holds() {
        let fx = Fixture::new();
        let targets =
            vec![SyncTarget { role: MailboxRole::Sent, server_name: "Sent Items".into() }];
        let mut backend = FakeBackend::default();
        let mut renumbered = fetch(vec![(10, raw("one")), (11, raw("two"))]);
        renumbered.uidvalidity_reset = true;
        backend.script(
            "Sent Items",
            vec![Ok(fetch(vec![(11, raw("one")), (12, raw("two"))])), Ok(renumbered)],
        );

        fx.run(&mut backend, &targets);
        assert_eq!(fx.rows("sent"), vec![11, 12]);
        let before = fx.row_ids("sent");

        let result = fx.run(&mut backend, &targets);

        assert_eq!(result.uidvalidity_resets, 1);
        assert_eq!(
            result.uid_rebound, 2,
            "the reset's own listing may not decline a rebind: both rows follow their message"
        );
        assert_eq!(result.saved, 0, "so neither message is inserted a second time");
        assert_eq!(fx.rows("sent"), vec![10, 11], "and no row is stranded on the old numbering");
        assert_eq!(
            fx.row_ids("sent"),
            vec![(before[0].0, 10), (before[1].0, 11)],
            "each message keeps the row id it had, which is what carries the thread and the blobs"
        );
    }

    /// The `!fetched.enumeration_complete` half of the same guard, on the same
    /// overlapping shape. A listing the server came back short on is already
    /// untrusted for pruning and is no more trustworthy for declining a rebind,
    /// so pass 2 must take both rebinds even though it lists 11, which one of
    /// the rows is parked on.
    ///
    /// The existing short-enumeration case in
    /// `an_empty_listing_falls_back_to_the_unconditional_rebind` cannot see
    /// this half either: its pass-2 listing is `[6542]` while the candidate row
    /// sits on 6540, so the membership test never fires.
    #[test]
    fn a_short_enumeration_rebinds_a_row_parked_on_a_uid_its_listing_holds() {
        let fx = Fixture::new();
        let targets =
            vec![SyncTarget { role: MailboxRole::Sent, server_name: "Sent Items".into() }];
        let mut backend = FakeBackend::default();
        let mut short = fetch(vec![(10, raw("one")), (11, raw("two"))]);
        short.enumeration_complete = false;
        backend.script(
            "Sent Items",
            vec![Ok(fetch(vec![(11, raw("one")), (12, raw("two"))])), Ok(short)],
        );

        fx.run(&mut backend, &targets);
        assert_eq!(fx.rows("sent"), vec![11, 12]);
        let before = fx.row_ids("sent");

        let result = fx.run(&mut backend, &targets);

        assert_eq!(
            result.uid_rebound, 2,
            "a listing that came back short may not decline a rebind either"
        );
        assert_eq!(result.saved, 0, "so neither message is inserted a second time");
        assert_eq!(fx.rows("sent"), vec![10, 11], "and no row is stranded on the old numbering");
        assert_eq!(
            fx.row_ids("sent"),
            vec![(before[0].0, 10), (before[1].0, 11)],
            "each message keeps the row id it had"
        );
    }

    /// #0117: a UIDVALIDITY reset whose detecting pass covered only part of the
    /// listing converges on the next *full* sync, one row per message and
    /// nobody parked on a recycled UID.
    ///
    /// Full sync, not next tick, and the difference is the fetch window rather
    /// than the skip list: `fetch_new_raw_on_session` builds it positionally
    /// (`listed.iter().rev().take(n)`), so it is the top of the mailbox, and
    /// the rows this repairs are below it by construction. Freeing their UIDs
    /// does not get them asked for on the next ordinary tick; it gets them
    /// asked for by the first pass whose window reaches them, which is a pass
    /// covering the whole listing. That is still the whole fix: before it, the
    /// recycled UID sat in the skip list and a full sync was blocked with
    /// everything else.
    ///
    /// The guard above degrades the gate on the pass that *detects* the reset,
    /// and that pass is the only one that sees it: `record_mailbox_cursor`
    /// writes the new UIDVALIDITY at the end of it, so `known.resolve` reports
    /// no reset from the next pass onward, while the refetch was capped at the
    /// window (100 UIDs on a quick tick, 50 on `mp sync`). Every row below that
    /// window used to keep its old-validity UID into a pass with the full
    /// listing trusted and no degradation, which is the case the guard exists
    /// for running without the guard.
    ///
    /// Here the recreated mailbox holds `three` on 11, `one` on 12 and `two` on
    /// 13, over a store holding `one` on 11 and `two` on 12. The reset pass has
    /// a window of one UID, so it can only rebind `two`. What it must not leave
    /// behind is `one` still claiming 11, which the new numbering has given to
    /// `three`: the claim goes into the next pass's skip list and hides
    /// `three` for good. The row is unbound instead, and the pass that
    /// downloads `one` on 12 gives it back the row it always had.
    ///
    /// The window is modelled by hand, as everywhere else here
    /// (`FakeBackend::fetch_targets` records `limit` and does not apply it),
    /// and so is pass 3's download set, which is scripted as the whole
    /// remaining listing because that is what a full sync asks for. What that set *would* be under a stale
    /// skip list is the other half, and
    /// `the_message_on_a_recycled_uid_is_downloaded_after_a_windowed_reset`
    /// derives it from the store rather than scripting it.
    #[test]
    fn a_reset_wider_than_the_window_converges_on_the_next_full_sync() {
        let fx = Fixture::new();
        let targets =
            vec![SyncTarget { role: MailboxRole::Sent, server_name: "Sent Items".into() }];
        let mut backend = FakeBackend::default();

        // Pass 2, the detecting pass: the mailbox was recreated under a new
        // UIDVALIDITY and lists three UIDs, of which a quick tick's window
        // covers only the top one.
        let mut renumbered = fetch(vec![(13, raw("two"))]);
        renumbered.listed = vec![11, 12, 13];
        renumbered.uidvalidity_reset = true;
        renumbered.state.uid_validity = Some(8);
        // Pass 3, a full sync: no reset left to report and the whole listing
        // trusted, and its window covers every UID. Both remaining UIDs come
        // back, because neither is claimed by a row any more. A windowed tick
        // here would still only reach the top of the mailbox: what the unbind
        // buys is that this pass is no longer blocked, not that a tick
        // suffices.
        let mut rest = fetch(vec![(11, raw("three")), (12, raw("one"))]);
        rest.listed = vec![11, 12, 13];
        rest.state.uid_validity = Some(8);
        backend.script(
            "Sent Items",
            vec![
                Ok(fetch(vec![(11, raw("one")), (12, raw("two"))])),
                Ok(renumbered),
                Ok(rest),
            ],
        );

        fx.run(&mut backend, &targets);
        assert_eq!(fx.rows("sent"), vec![11, 12]);
        let before = fx.row_ids("sent");
        let (one_row, two_row) = (before[0].0, before[1].0);

        let reset = fx.run_with(&mut backend, &targets, 1, false);

        assert_eq!(reset.uidvalidity_resets, 1);
        assert_eq!(reset.uid_rebound, 1, "the window covered one of the two rows");
        assert_eq!(
            fx.row_ids("sent"),
            vec![(one_row, -one_row), (two_row, 13)],
            "`two` follows its message, and `one`, which the window did not reach, is unbound \
             rather than left claiming a UID the new numbering gave to another message"
        );
        assert_eq!(
            ingest::known_uids_with_cursor(&fx.store, "acct", "sent").unwrap().uidvalidity,
            Some(8),
            "the detecting pass still records the new UIDVALIDITY, so no later pass reports a \
             reset and the fix may not depend on one"
        );
        let known = ingest::known_uids_with_cursor(&fx.store, "acct", "sent").unwrap();
        assert!(
            !known.uids.contains(&11),
            "and 11 is out of the skip list, so a pass whose window reaches it downloads what \
             the server actually holds there"
        );

        let after = fx.run(&mut backend, &targets);

        assert_eq!(after.uid_rebound, 1, "`one` follows its own row onto its new UID");
        assert_eq!(after.saved, 1, "and only `three`, which is genuinely new, is inserted");
        assert_eq!(fx.rows("sent"), vec![11, 12, 13]);
        assert_eq!(
            fx.rows_for("sent", "<one@example.com>"),
            vec![(one_row, 12)],
            "one message, one row, and the row it started on: the thread assignment and the \
             blob references ride on that id"
        );
        assert_eq!(fx.rows_for("sent", "<two@example.com>"), vec![(two_row, 13)]);
        let three = fx.rows_for("sent", "<three@example.com>");
        assert_eq!(three.len(), 1, "and the message on the recycled UID has a row of its own");
        assert_eq!(three[0].1, 11);
    }

    /// The severe half of #0117, pinned where it actually bites: not the
    /// duplicate row, but the message that becomes undownloadable behind it.
    ///
    /// `known_uids` is `SELECT uid FROM messages`, so a row still claiming a
    /// recycled UID puts that UID in the skip list, pass 2 of the fetch never
    /// asks for it, and the server keeps listing it so `vanished_uids` never
    /// prunes the row either. The message the server holds there is then
    /// permanently missing, and a full sync does not clear it: the window is
    /// not what is wrong, the skip list is.
    ///
    /// This is the one test that hands the backend the *mailbox* and lets it
    /// derive its download set from the store's skip list, which is what makes
    /// "`three` was never asked for" observable rather than scripted away.
    ///
    /// The pass that does the downloading is a full sync: the backend is
    /// offered the whole recreated listing, which is what a window covering
    /// the mailbox asks for. A windowed tick would not reach 11, because the
    /// window is the top of the listing; what the unbind changes is that the
    /// skip list no longer hides 11 from the pass that does reach it.
    #[test]
    fn the_message_on_a_recycled_uid_is_downloaded_after_a_windowed_reset() {
        let fx = Fixture::new();
        let targets =
            vec![SyncTarget { role: MailboxRole::Sent, server_name: "Sent Items".into() }];
        let mut backend = FakeBackend { honour_skip_list: true, ..Default::default() };

        let mut renumbered = fetch(vec![(13, raw("two"))]);
        renumbered.listed = vec![11, 12, 13];
        renumbered.uidvalidity_reset = true;
        renumbered.state.uid_validity = Some(8);
        // The whole recreated mailbox, offered to every later pass: what comes
        // back is what the skip list does not already hold.
        let mut whole = fetch(vec![(11, raw("three")), (12, raw("one")), (13, raw("two"))]);
        whole.listed = vec![11, 12, 13];
        whole.state.uid_validity = Some(8);
        backend.script(
            "Sent Items",
            vec![
                Ok(fetch(vec![(11, raw("one")), (12, raw("two"))])),
                Ok(renumbered),
                Ok(whole),
            ],
        );

        fx.run(&mut backend, &targets);
        fx.run_with(&mut backend, &targets, 1, false);
        fx.run(&mut backend, &targets);

        let three = fx.rows_for("sent", "<three@example.com>");
        assert_eq!(
            three.len(),
            1,
            "the message the new numbering put on the recycled UID is downloaded, not left \
             behind a stale skip-list entry"
        );
        assert_eq!(three[0].1, 11);
        assert_eq!(
            fx.rows_for("sent", "<one@example.com>").len(),
            1,
            "and the message that was parked on that UID is not duplicated"
        );
        assert_eq!(fx.rows("sent"), vec![11, 12, 13], "three messages, three rows");
    }

    /// The other edge of the same unbinding: a row is taken off its UID only
    /// when the new listing holds that UID, because that is the only case where
    /// the claim hides a message.
    ///
    /// A row whose UID the server does not list blocks nothing, and unbinding
    /// it would park it on the move sentinel that no prune touches
    /// (`vanished_uids` skips `uid <= 0`), stranding a row whose message the
    /// recreated mailbox simply no longer holds.
    #[test]
    fn a_reset_leaves_a_row_alone_when_the_new_listing_has_no_uid_for_it() {
        let fx = Fixture::new();
        let targets =
            vec![SyncTarget { role: MailboxRole::Sent, server_name: "Sent Items".into() }];
        let mut backend = FakeBackend::default();

        // The recreated mailbox lists 12 and 13 only: 11, which `one` sits on,
        // is below its numbering.
        let mut renumbered = fetch(vec![(13, raw("two"))]);
        renumbered.listed = vec![12, 13];
        renumbered.uidvalidity_reset = true;
        renumbered.state.uid_validity = Some(8);
        backend.script(
            "Sent Items",
            vec![Ok(fetch(vec![(11, raw("one")), (12, raw("two"))])), Ok(renumbered)],
        );

        fx.run(&mut backend, &targets);
        let before = fx.row_ids("sent");
        let (one_row, two_row) = (before[0].0, before[1].0);

        fx.run_with(&mut backend, &targets, 1, false);

        assert_eq!(
            fx.row_ids("sent"),
            vec![(one_row, 11), (two_row, 13)],
            "`two` was unbound off 12, which the listing holds, and rebound onto 13; `one` \
             keeps 11, which the listing does not hold, so the prune can still reach it"
        );
    }

    // -----------------------------------------------------------------------
    // The arrival-mark arithmetic itself
    // -----------------------------------------------------------------------

    /// #0074: what the download covered is not what the pass wrote. A UID that
    /// was fetched and not ingested pulls the persisted mark under itself, even
    /// when the download reported the pass complete.
    #[test]
    fn an_unwritten_uid_pulls_the_mark_below_itself() {
        // The complete-looking case the bug lived in: no mark, so the next pass
        // would have derived a floor above the message it never wrote.
        assert_eq!(mark_below_unmet(None, &[105]), Some(104));
        // The lowest one wins; everything above it is an arrival again too.
        assert_eq!(mark_below_unmet(None, &[110, 105, 107]), Some(104));
        // A mark the download already reported can only be lowered.
        assert_eq!(mark_below_unmet(Some(100), &[105]), Some(100));
        assert_eq!(mark_below_unmet(Some(200), &[105]), Some(104));
        // Nothing unwritten changes nothing, which is how the gate reopens.
        assert_eq!(mark_below_unmet(None, &[]), None);
        assert_eq!(mark_below_unmet(Some(100), &[]), Some(100));
        // The bottom of the mailbox: every listed UID is an arrival.
        assert_eq!(mark_below_unmet(None, &[1]), Some(0));
    }
}
