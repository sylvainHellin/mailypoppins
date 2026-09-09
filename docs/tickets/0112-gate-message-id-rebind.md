---
id: 0112
title: Rebind through the message_id index only when the UID cannot be live
type: bug
priority: now
status: done
created: 2026-09-09
---

First ticket of [docs/plans/sync-skip-list.md](../plans/sync-skip-list.md), and the one that stops the reported pain.
The other four (#0113 to #0116) are independent of it and of each other, and are not touched here.

## Problem

Every sync tick re-downloaded the same message bodies and never converged.
The TUM account logged an identical `Store fetch for 'Sent Items': 16 new, 84 already ingested` on every pass, and one tick spent 160 seconds re-downloading about 34 MB.

`ingest_in_tx` finds the row a message belongs in with two lookups.
The first is identity, `(account, mailbox, uid)`.
When that misses, the second is the `message_id` index, and a hit rewrites `uid` on the matched row.

The fallback exists to absorb a UIDVALIDITY reset and its doc comment says so, but nothing enforced that.
It fired on any UID the store did not hold whose `Message-ID` it did hold in that mailbox, whether or not the server had renumbered anything.
So a mailbox holding several server-side copies of one message could not be mirrored: the store kept one row for all of them, that row's `uid` flipped to whichever copy was ingested last, and every other copy was absent from the skip list on the next pass, was reported new, was downloaded in full, and rebound the row back.
Nothing errored and nothing was logged as a failure, so the loop was stable forever and survived a restart.

The magnitude in the live TUM store: six messages had 6, 5, 4, 3, 2 and 1 server-side copies, and `copies - 1` summed over them is 5+4+3+2+1+1 = 16, exactly the stable "16 new".
The fix therefore has to handle N copies of one `Message-ID`, not two.

The non-circular evidence is the ingest that leaves nothing behind.
Ten UIDs (6540, 6542, 6543, 6544, 6545, 6546, 6548, 6549, 6550, 6551) ingested with a `committed` span on every pass and existed nowhere in `messages` afterwards.
An ingest that commits and leaves no row must have UPDATEd another row.

## Approach

Ingest takes the rebind decision from a policy the caller supplies.

`RebindPolicy::Always` is the behaviour ingest always had, and is what `ingest_message` still does, so the roughly thirty call sites that build an `IngestInput` are untouched.
`RebindPolicy::UnlessListed(&HashSet<i64>)` is the gate: a candidate row may be rebound only when the UID it is parked on is not one the server is currently listing for that mailbox.
The sync engine reaches it through the new `ingest_message_with_policy`, which is a second entry point rather than a field on `IngestInput` because exactly one of those call sites holds a listing to decide with.

`MailboxFetch` gains `listed`, every UID the server enumerated for the mailbox on this pass, which `fetch_new_raw_on_session` already computed and threw away.

### Eligibility is part of candidate selection

The plan proposed gating the existing `ORDER BY id LIMIT 1` lookup after the fact.
That is wrong and would create a permanent phantom row.

Today at most one row per `(mailbox, message_id)` exists precisely because the rebind collapses them, and this fix removes that guarantee, so from the first pass afterwards the query has several candidates and `LIMIT 1` returns an arbitrary one.
The concrete break: two rows parked on the `-id` move sentinel in one mailbox under one `Message-ID`.
Ingesting the first listed UID picks row 1 and rebinds it.
Ingesting the second still picks row 1 by `LIMIT 1`, whose UID is now live, so the rebind is declined and a third row is INSERTed, stranding row 2 on `-id2` forever: `vanished_uids` skips `uid > 0` only, so a negative UID is never pruned and nothing rebinds it again.

So the lookup reads every candidate in preference order and takes the first eligible one, in `rebindable_row`.
`ORDER BY (uid >= 0), id` is the preference: a row waiting on the move sentinel is always rebindable and is offered ahead of one parked on a number the server may still be using.
The ineligible set is not pushed into the SQL as a `NOT IN`, because it is the server's whole UID listing, thousands of values on a large mailbox and well past what a bound-parameter list may carry; the candidate set is one row per copy the mailbox holds.

### Where the gate degrades, and what survives the degradation

Two kinds of pass cannot supply a listing the gate may decline a rebind with, and the engine writes the rule once.

A UIDVALIDITY reset renumbered the mailbox, so a listed UID says nothing about which message wears it and a row whose old number was recycled must still follow its message (#0038).
A short enumeration listed fewer UIDs than the mailbox announced under EXISTS, a listing already untrusted for pruning and no more trustworthy here.
Both seed the gate with an empty set, which is the unconditional rebind.

What survives the degradation is that the set grows as the pass ingests: a row this pass has already moved onto a UID is off limits for the rest of it.
That is what keeps N copies from collapsing back onto the lowest-id row when a reset re-downloads all of them at once.

The two locally written UID shapes need no special case.
The `-id` move sentinel is negative and the synthetic `graph_uid` is a 63-bit hash, and neither can appear in a `Vec<u32>` server listing, so a row parked on either is always rebindable.

A declined rebind logs once at warn, naming the mailbox, both UIDs and the `Message-ID`.

## Acceptance criteria

- N UIDs the server lists under one `Message-ID` produce N rows after one pass, and a second pass over the unchanged mailbox reports nothing new and moves no row's `uid`.
- A UIDVALIDITY reset still rebinds through the `Message-ID` index, keeps the row id, the thread and the blob refcounts, still reports `uid_rebound`, and maps N copies onto N rows rather than one.
- A row parked on the `-id` move sentinel is still rebound onto its real UID, and with duplicates present no row is stranded and no extra row is invented.
- A `graph_uid` sent-copy placeholder is still rebound onto the real UID.
- An empty listing degrades to the unconditional rebind, and so does an enumeration the server came back short on.
- `cargo test` passes.

## Consequence

The insert branch creates rows where the old code created none.
After this ships the 16 TUM and 2 Proton second copies ingest as ordinary rows, and the Sent list shows duplicates that were on the server all along and were hidden by the collapse.
Removing them means deleting messages on the server, which is a user decision; stopping new ones is #0116.

The blob refcounts for those hashes are worth one look once the duplicate rows exist: every duplicate body was written and deduped many times without the store ever gaining a second row.

## Files

- `src/ingest.rs`: `RebindPolicy`, `ingest_message_with_policy`, `rebindable_row`.
- `src/sync/mod.rs`: `MailboxFetch::listed`.
- `src/imap_client/fetch.rs`: fills `listed` on all three return paths.
- `src/sync/engine.rs`: builds the set per target, grows it per ingest, passes the policy down.
