---
id: 0117
title: The rebind gate runs undegraded on the passes that finish a windowed UIDVALIDITY reset
type: bug
priority: next
status: open
created: 2026-09-09
---

Found while pinning the seeding guard of [#0112](0112-gate-message-id-rebind.md), not by that ticket's own work.
No fix here: the witness is `a_reset_wider_than_the_window_leaves_a_straggler_the_next_pass_duplicates` in `src/sync/engine.rs`, which documents the current behaviour and is what the fix changes.

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

The witness test drives the real engine over three passes.
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

- A reset whose detecting pass covers only part of the listing converges: each message ends on one row, no row is stranded on a recycled UID, and no message is left undownloadable behind a stale skip-list entry.
- The #0112 guarantee is untouched: N server-side copies of one `Message-ID` still get N rows, and a second pass over an unchanged mailbox still moves no row.
- The witness test above is rewritten as an assertion of the fixed behaviour.
