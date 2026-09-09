---
id: 0115
title: Warn when a fetch downloads the same messages pass after pass
type: feature
priority: now
status: done
created: 2026-09-09
---

Fifth and last ticket of the sync-skip-list group, after [#0112](0112-gate-message-id-rebind.md), [#0117](0117-rebind-gate-after-a-windowed-reset.md), [#0116](0116-one-outbox-drain-per-account.md), [#0114](0114-drain-mutation-queue-at-tail.md) and [#0113](0113-bound-the-body-fetch.md).
Deliberately written after #0112, so the detector is not tuned against the bug it would have caught.

See the Resolution section at the end for what shipped.

## Problem

For weeks, every pass on the TUM account logged the same line for the same mailbox:

```
Store fetch for 'Sent Items': 16 new, 84 already ingested
```

The same 16 UIDs, about 34 MB, downloaded again on every tick, ingested into rows that already held them, for weeks.
#0112 is why it happened; this ticket is about the fact that nothing said so.

Nothing errored, so nothing was logged above info.
The counters a pass reports are all per pass, and a pass that downloads 16 messages and writes none of them as new looks, in isolation, exactly like a pass that had 16 messages to catch up on.
The one thing that distinguishes the two is the pass before it, and no trace of a converged pass survived it: the cursor moves, the skip list grows, and neither says "this is the second time".

So the failure mode is silent by construction, and the next instance of it will be too unless a pass can compare itself to the last one.

## Resolution

A complete pass owes the next one nothing.
That is the whole argument: when a pass has seen the whole mailbox, downloaded every new UID it owed, written all of them and is not recovering from a renumbering, then whatever it downloaded is now in the store, and the next such pass has no reason to download it again.
A repeat is therefore not a slow mailbox or a big backlog; it is something upstream refusing to converge.

The detector is one `meta` row per mailbox, `nonconverging:{role}`, holding `{hash}:{count}:{streak}`.
The store is per account (`config::store_path`), so the role is the whole key, as it is for the other per-mailbox markers here.
`fingerprint` hashes the sorted, deduplicated UID set with `DefaultHasher` and carries the count alongside, so a hash collision still has to agree on how many messages there were.
`advance_streak(prev, now)` is the entire state machine and is pure: the same fingerprint continues the streak, anything else starts a new one at 1.

Which passes participate is the load-bearing half.
A pass qualifies when it is not a dry run and `enumeration_complete && !download_incomplete && bodies_complete && !uidvalidity_reset` and it wrote everything it downloaded (`unmet` empty).
A pass that fails any of those is *expected* to leave work behind: a body pass cut at its deadline (#0113), a short enumeration or a message the store rejected all owe the next pass a re-download, so a repeat proves nothing about them.
Such a pass is silent and leaves the row exactly as it found it, rather than resetting a streak the complete passes around it built up.

One of those is silent for a second reason, and the empty `unmet` is not enough to catch it.
A message the store has given up on after `MAX_INGEST_ATTEMPTS` ([#0074](0074-arrival-mark-misses-ingest-failures.md)) drops out of `unmet` by design, so every later pass looks complete while the server still lists a UID that has no row and gets downloaded again under an unchanging fingerprint.
That is a permanent repeat on a state #0074 declares expected, so the pass tracks the give-up separately (`gave_up`) and disqualifies itself on it, exactly as it does for the shapes above.

The detector rides the IMAP engine only.
`graph.rs` still runs its own loop (the parity half of [#0059](0059-syncbackend-trait.md) is parked with the Graph backend) and builds a `SyncResult` with `non_converging` always empty; the account that motivated this is IMAP, so the Graph half waits for that parity work rather than duplicating the state machine by hand.

Two shapes delete the row instead:

- a `uidvalidity_reset` pass, in the same branch that drops the modseq and the ingest-failure counters, because the fingerprint describes UIDs the server has just renumbered and comparing across the reset compares two different mailboxes;
- a qualifying pass with nothing new, because that is the fetch converging, which is precisely what the detector is waiting to see.

At streak 2 the mailbox is reported, and it keeps being reported for as long as the streak holds.
The log line is throttled to streaks 2, 3, then every tenth, so a bug that persists for a week stays visible without one line per tick:

```
[sync] 'Sent Items' on account 'tum' downloaded the same 16 message(s) (uids 84..99) on 2 consecutive passes; the fetch is not converging, see docs/tickets/0115-warn-on-a-non-converging-fetch.md
```

The status is not only in the log.
`SyncResult.non_converging` carries the server names, the TUI status line gains `, fetch not converging on {names} (see the log)` where the deferred-prune and fetch-deadline suffixes go, and `drained_sync_level` treats that suffix like the rollback marker, so the line is a warning rather than a green "Synced: 16 new, 84 existing".
`mp sync` prints one `⚠` line per affected mailbox.
The exit code is unchanged, since nothing failed: what is wrong is that the work repeats.

## What to do when you see this

1. Run `mp sync -v` on the account and watch the mailbox the warning names.
2. Look for the `[ingest]` rebind-declined warn from #0112 in the same pass, and for repeated `Store fetch for '<mailbox>': N new, M already ingested` lines with an unchanging `N`.
3. If the UIDs in the warning are the same ones every pass, the store is not recording what it downloaded: a row is parked on a UID that belongs to another copy of the message, or a skip-list entry survived a renumbering (#0117).
4. File a ticket with the warn line, the `[ingest]` lines around it, and `sqlite3 <store> "SELECT uid, message_id FROM messages WHERE mailbox = '<role>' ORDER BY uid"` for the UID range named.

A single warning after a UIDVALIDITY reset or a manual store edit is not a bug: the marker is cleared on a reset, so the first two passes after one are the detector re-learning the mailbox.

## Witness tests

- `a_streak_continues_only_on_an_identical_fingerprint` (`sync/engine.rs`): the pure half, order-independence of the UID set, the count as part of the identity, the reset on a different set, the 2/3/every-tenth throttle, and the malformed-row parse.
- `a_pass_that_downloads_the_same_uids_again_reports_a_fetch_that_is_not_converging`: one download proves nothing, the second reports, the third keeps reporting, a different set converges.
- `a_short_repeat_pass_neither_counts_nor_resets_the_streak`: a truncated pass repeating itself is neither a repeat nor a reset.
- `a_message_the_store_gave_up_on_is_not_a_fetch_that_fails_to_converge`: a message rejected past `MAX_INGEST_ATTEMPTS` is downloaded on every pass forever and still never reported.
- `a_uidvalidity_reset_clears_the_convergence_marker` and `a_pass_with_nothing_new_clears_the_convergence_marker`: the two delete paths.
- `a_non_converging_suffix_downgrades_the_sync_status_to_warning` (`tui/bg.rs`): the suffix cannot ride a green status line.

## Acceptance criteria

- Two consecutive complete passes downloading the same UID set produce a warn line and a warning-level status line.
- A truncated, short or dry-run pass changes nothing.
- A renumbering and a pass with nothing new both clear the marker.
- `mp sync`'s exit code is unchanged.
- `cargo test` passes.

## Files

- `src/sync/engine.rs`: `NONCONVERGING_PREFIX`, `fingerprint`, `advance_streak`, `streak_is_loud`, `parse_streak_row`, `clear_nonconverging`, `note_download_fingerprint`, the qualifying check in the phase-3 loop (including the `gave_up` flag), and the tests.
- `src/sync/mod.rs`: `SyncResult.non_converging`.
- `src/tui/helpers.rs`: `NON_CONVERGING_MARKER` and the `finish_sync` suffix.
- `src/tui/bg.rs`: `drained_sync_level` reads the second marker.
- `src/main.rs`: the `mp sync` report line.
