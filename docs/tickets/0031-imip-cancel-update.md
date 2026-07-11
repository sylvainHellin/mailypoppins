---
id: 0031
title: iMIP cancellations and updates (CANCEL / SEQUENCE bump)
type: feature
priority: later
status: open
created: 2026-07-11
---

Follow-up to the v1 iMIP work. Design: [calendar-invites](../plans/calendar-invites.md) (D8, v1/v2 boundary). Out of scope for the first cut; captured so it is not lost.

## Scope (v2)

- Handle received `METHOD:CANCEL`: mark the local invite cancelled, update the card/badge, keep the sidecar `.ics`.
- Handle updates: a re-issued invite with a bumped `SEQUENCE` supersedes the prior one; reconcile against the existing `UID`.
- Send-side updates/cancellations from `mp` (re-send with bumped `SEQUENCE` / `METHOD:CANCEL`).
- Per-occurrence responses and creation of recurring invites (v1 handles whole series only, D6).

## Related

- Depends on [#0027](0027-imip-receive-parse.md) (already tags `method: CANCEL` but takes no action in v1).
