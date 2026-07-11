---
id: 0030
title: iMIP REPLY reconciliation - organizer-side attendee tracking
type: feature
priority: next
status: open
created: 2026-07-11
---

Organizer-side RSVP tracking. Design: [calendar-invites](../plans/calendar-invites.md) (D4). Depends on [#0027](0027-imip-receive-parse.md) (parse) and [#0028](0028-imip-send-invite.md) (sent invites to match against).

Incoming `text/calendar; method=REPLY` parts are parsed during sync and matched by event `UID` to the sent invite, so the organizer sees who accepted/declined.

## Scope

- During sync, when parse yields a `method=REPLY`: extract `UID` and the replying `ATTENDEE`'s `PARTSTAT`.
- Find the local sent invite whose `event.uid` matches (Sent / invite index).
- Update that invite's `attendees[].status` for the matching address by rewriting the `event:` frontmatter block in place (see `docs/lessons-learned.md` on in-place YAML rewrites — rewrite only the block, preserve the body).
- The REPLY email itself is not rendered as an invite card ([#0027](0027-imip-receive-parse.md) branch).
- Preview card ([#0029](0029-imip-rsvp-reply.md)) reflects live attendee statuses.
- **Idempotent + re-runnable over the whole mailstore.** The `event:` block is local derived state, reconstructible from IMAP-visible messages alone (design §3, multi-machine consistency). Reconciliation must be idempotent (re-processing a REPLY changes nothing the second time) and runnable over all existing messages, not only newly fetched ones — wire it into sync **and** expose a full re-scan command mirroring `mp contacts rebuild` (e.g. `mp calendar rebuild`; exact name implementer's choice). Two machines syncing the same account then converge on identical event state with no machine-to-machine sync.

## Honest caveat (document, do not fix here)

Without Graph, this updates only the **local** mirror: the user's Exchange calendar is not touched, and sent invites create no event in the user's own Exchange calendar. Server-side sync is the Graph backend ([#0036](0036-graph-sync-backend.md)), gated on admin approval ([#0035](0035-graph-admin-approval.md)).

## Acceptance criteria

- A `REPLY` arriving for a locally-sent invite flips the matching attendee's status in frontmatter and in the preview card.
- A `REPLY` with no matching local invite is handled gracefully (logged, not rendered as an invite).
