---
id: 0117
title: The rebind gate runs undegraded on the passes that finish a windowed UIDVALIDITY reset
type: bug
priority: now
status: done
created: 2026-09-09
---

Found while pinning the seeding guard of [#0112](0112-gate-message-id-rebind.md), not by that ticket's own work.

Filed at `priority: next` and raised to `now` when it was fixed.
The duplicate row is cosmetic; the message that becomes permanently undownloadable behind the stale skip-list entry is silent mail loss on a mirror whose whole contract is that the server is truth, and no pass, `mp sync` or full TUI sync, converges it.
That belongs in the same band as #0112 itself.

See the Resolution section at the end for what shipped.

## Problem

The engine seeds the rebind gate with an empty set when `fetched.uidvalidity_reset || !fetched.enumeration_complete`, which is the degradation a renumbering needs: a row whose old number was recycled must still be allowed to follow its message.
Only the pass that detects the reset gets that degradation, and that pass may not have covered the mailbox.

The chain:

1. `record_mailbox_cursor` (`src/sync/engine.rs`) writes the server's new UIDVALIDITY at the end of the detecting pass, so `known.resolve` (`src/imap_client/fetch.rs`) reports no reset from the next pass onward.
2. The reset refetch is still capped by the window, `listed.iter().rev().take(n)` (`src/imap_client/fetch.rs`), 100 UIDs on a quick tick.
   The warning it logs says "refetching the whole window", and the window is not the mailbox.
3. Every row below that window keeps its old-validity UID and is rebound on a later pass, with the full listing trusted and no degradation.

So the straggler's rebind is decided by exactly the gate the reset is supposed to switch off.
When the new numbering reuses a number the straggler is parked on, and a recreated mailbox restarting low over a store holding those same low numbers is the shape that does, the candidate is declined and the message is inserted as a second row.

The stale row then keeps that UID in the skip list, so the message the server actually holds there is never downloaded.
The store ends up with one message twice and another missing, and it stays that way until the recycled UID vanishes server-side.

## Reproduction

The witness test drove the real engine over three passes, and the fix rewrote it into `a_reset_wider_than_the_window_converges_on_the_next_full_sync`.
The store holds `one` on 11 and `two` on 12; the mailbox is recreated holding `three` on 11, `one` on 12 and `two` on 13.
The detecting pass has a window of one UID, rebinds `two` onto 13 and leaves `one` parked on 11.
The next pass downloads `one` on 12, finds its row on 11, which that pass lists, declines the rebind, and inserts.
Result: `uid_rebound == 0`, `saved == 1`, rows `[11, 12, 13]`, two rows carrying `<one@example.com>`, and 11 still in the skip list.

## Scope

Pre-#0112 the straggler rebound unconditionally and the mailbox converged, so this is a regression of that ticket, bounded by its trigger: a UIDVALIDITY reset, a mailbox larger than the window, and an overlap between the old and the new numbering.

A neighbouring hazard is older than #0112 and is not this ticket: when a recycled UID falls inside the reset pass's own window, the identity lookup hits the row parked on it and overwrites that row's message with a different one.
Any fix here should say what it does about that.

## Approach, not decided

Options, in rough order of appetite:

- Carry the reset forward: record on the cursor that the mailbox is mid-reset, and keep the gate degraded until a pass whose window covered the whole listing.
  That is the narrowest statement of the invariant, and it is the one the degradation already means.
- Lift the cap on the pass that detects a reset, so the refetch really is the whole mailbox.
  Simple, and it makes one tick after a reset arbitrarily long on a large mailbox.
- Treat a row whose UID predates the current UIDVALIDITY as always rebindable, which needs the row to carry the validity it was written under.

## Acceptance criteria

- A reset whose detecting pass covers only part of the listing converges on the next full sync: each message ends on one row, no row is stranded on a recycled UID, and no message is left undownloadable behind a stale skip-list entry.
- The #0112 guarantee is untouched: N server-side copies of one `Message-ID` still get N rows, and a second pass over an unchanged mailbox still moves no row.
- The witness test above is rewritten as an assertion of the fixed behaviour.

## Resolution

The detecting pass now says what it verified before it leaves.
At the tail of a pass that reported a reset, `ingest::unbind_rows_on_uids` takes every row still parked on a listed UID the pass did not ingest off that UID and onto the `-id` sentinel `store::write::move_row` already uses for a row holding no server UID.
The UID is then free, so the message the new numbering put there is downloadable again, and the row is rebindable by construction, so its own message takes it back when a pass downloads it.

That pass is the next *full* sync, not the next tick, and the commit message and the first changelog wording of this ticket both overstated it.
`fetch_new_raw_on_session` builds its download window positionally (`listed.iter().rev().take(n)`), so the window is the top of the mailbox and the rows unbound here are below it by construction: freeing a UID does not get it requested on an ordinary windowed tick.
What the fix changes is that a pass covering the whole listing is no longer blocked, where before the recycled UID sat in the skip list and blocked the full sync exactly as it blocked every other pass, which is the difference between a store that heals and one that does not.

Rows on UIDs the server does not list are left where they are.
They block nothing, and unbinding them would park a row whose message the recreated mailbox no longer holds on a sentinel no prune touches (`vanished_uids` skips `uid <= 0`).

The unbind set is `listed \ ingested`, which on a windowed pass over a large mailbox is nearly the whole mailbox, and that cost is accepted rather than narrowed.
After a UIDVALIDITY change every cached UID is a number from a numbering that no longer exists, so a row the pass did not itself verify has no binding worth keeping, and a stale one is worse than a re-download: `apply_flags` would write the server's flags for the new occupant onto it, and a queued move or delete would act on the wrong message.
The price is paid by every row below the window: it moves to the `-id` sentinel, so it is invisible to the prune and to the flag pass until it is rebound, and the next full sync re-downloads a body the store already holds in order to rebind it.
The row itself, with its id, its thread assignment and its blob references, is never lost, and the mailbox is whole again after one full sync.

The unbinding runs after the ingest loop rather than before it, which is load-bearing in two directions.
Before the loop it would take every row off its listed UID, the #0112 seeding guard would have nothing left to decline, and its reset half would become dead code that could be deleted with the suite green.
After the loop the guard still decides every rebind the pass takes, and the unbinding sees only what the pass could not verify, which is exactly the straggler set.

The options that were weighed and dropped:

- Carry the reset across passes by withholding the new UIDVALIDITY from the cursor until a pass covers the listing.
  It does not converge: the straggler is below the window by construction, so no later windowed pass ever revisits it, and meanwhile `KnownUids::resolve` empties the skip list on every pass and the window is re-downloaded each tick.
- Lift the window cap on the detecting pass.
  It converges, at the cost of one tick that downloads an entire mailbox, and every byte of it is a body the store already holds: after a renumbering the store is missing numbers, not messages.
- Give each row the UIDVALIDITY it was written under.
  The cleanest statement of the invariant, and it needs a column on `messages`, which under the drop-and-rebuild contract makes every user re-download every mailbox to fix a bug that costs one message per recycled UID.

The neighbouring hazard named above is unchanged and still open: when a recycled UID falls inside the reset pass's *own* window, the identity lookup finds the row parked on it and overwrites that row's message with the different one the server now holds there.
The mailbox still converges, because the overwritten message is downloaded again on a later pass and inserted, but it loses its row and therefore its thread assignment and its blob references.
The unbinding cannot reach it: the UID is one the pass ingested, which is the strongest verification there is, and it is the identity lookup rather than the rebind gate that does the damage.

Tests in `src/sync/engine.rs`:

- `a_reset_wider_than_the_window_converges_on_the_next_full_sync`, the rewritten witness: each message on one row, `one` back on the row id it started with, and 11 out of the skip list at the end of the detecting pass. Its third pass is scripted with the whole remaining listing, which is what a full sync asks for; a windowed tick would not reach 11.
- `the_message_on_a_recycled_uid_is_downloaded_after_a_windowed_reset`, the severe half.
  It is the one test that lets the fake backend derive its download set from the store's skip list (`FakeBackend::honour_skip_list`), so "`three` was never asked for" is observable rather than scripted away.
- `a_reset_leaves_a_row_alone_when_the_new_listing_has_no_uid_for_it`, the other edge.

Both halves of the #0112 seeding guard still bite: deleted in turn, `a_reset_rebinds_a_row_parked_on_a_uid_its_own_listing_still_holds` and `a_short_enumeration_rebinds_a_row_parked_on_a_uid_its_listing_holds` each report `uid_rebound` 0 against the 2 they ask for.
