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

One multi-tenant app registration (any Entra tenant plus personal accounts) in the dedicated `mailypoppins.onmicrosoft.com` tenant, with public client flows enabled and the delegated Graph permissions `Mail.Read`, `Mail.ReadWrite`, `Mail.Send`, `offline_access`, `User.Read` and `Calendars.ReadWrite`.
The EVOQS tenant consents to it as an outside app, with Sylvain as its admin.
Other organisations need a verified publisher (EVOQS GmbH via Partner Center, publisher domain `mailypoppins.com`) before their users can consent without an admin.
Publisher verification is not needed for a single-tenant app consented by its own admin.

For the TUM tenant, still blocked:

- Azure app **publisher verification** / app registration hardening as required for admin consent.
- Add the calendar/contacts scopes to the Azure app "API permissions" (`Calendars.ReadWrite`, `Contacts.Read`) — see `docs/exchange-setup.md` Step 3 pattern.
- Obtain **tenant admin consent** for the TUM M365 tenant.
- Document the approved scope set and consent state in `docs/auth.md` / `docs/exchange-setup.md`.

## Status

App registered 2026-10-21: client ID `439d7ebd-7898-4df2-b2d6-88bb61e2597b`, tenant `365f441f-42ba-4b36-851f-8b124c7becbb` (mailypoppins).
EVOQS: consent pending, granted by Sylvain at the first device-code sign-in.
Publisher verification: not started.
TUM: admin approval unobtainable; revisit if the tenant policy changes.

## Blocks

- [#0036](0036-graph-sync-backend.md) Graph sync backend.
