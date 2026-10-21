---
id: 0035
title: Microsoft Graph API admin approval + Azure app verification
type: chore
priority: now
status: in-progress
created: 2026-07-11
---

Unparked 2026-10-21 (decision log): the EVOQS tenant is the first live Graph target, and Sylvain administers it, so he grants the consent himself; the TUM tenant stays out of reach.

Gating task for any Microsoft Graph-based sync backend. Design context: [calendar-invites](../plans/calendar-invites.md) (D1), [tui-restructure-views](../plans/tui-restructure-views.md) (D12).

The TUM M365 tenant requires **IT-admin approval** for Graph API access, which is currently unobtainable. Until that is resolved, all calendar work is iMIP-only (no Graph). This ticket tracks the operational/administrative path so the code work ([#0036](0036-graph-sync-backend.md)) is unblocked when — and only when — approval lands.

## Scope (operational, not code)

For the EVOQS tenant: an app registration with public client flows enabled and the delegated Graph permissions `Mail.Read`, `Mail.ReadWrite`, `Mail.Send`, `offline_access`, `User.Read` and `Calendars.ReadWrite`, admin consent granted by Sylvain.
Publisher verification is not needed for a single-tenant app consented by its own admin.

For the TUM tenant, still blocked:

- Azure app **publisher verification** / app registration hardening as required for admin consent.
- Add the calendar/contacts scopes to the Azure app "API permissions" (`Calendars.ReadWrite`, `Contacts.Read`) — see `docs/exchange-setup.md` Step 3 pattern.
- Obtain **tenant admin consent** for the TUM M365 tenant.
- Document the approved scope set and consent state in `docs/auth.md` / `docs/exchange-setup.md`.

## Status

EVOQS: in progress, app registration and consent being done by Sylvain (2026-10-21).
TUM: admin approval unobtainable; revisit if the tenant policy changes.

## Blocks

- [#0036](0036-graph-sync-backend.md) Graph sync backend.
