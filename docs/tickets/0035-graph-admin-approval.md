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
EVOQS: consented for the whole organisation by Sylvain at the first device-code sign-in (2026-10-21); the `evoqs` Graph account syncs. For the onboarding test, delete the app from EVOQS's Enterprise applications after verification and consent again as a new organisation would.
Publisher verification: `mailypoppins.com` is a verified custom domain in the mailypoppins tenant, the native admin `admin@mailypoppins.com` has MFA, app branding is saved (logo, home page, privacy URL, publisher domain), the Partner Center enrolment of EVOQS GmbH (contact `admin@mailypoppins.com`) is submitted (2026-10-21), and Sylvain completed the Verified ID check (2026-10-22); Microsoft's review of the enrolment is pending.
Contact verification passed; business verification waits on Microsoft; identity verification waits on Sylvain's Verified ID in Microsoft Authenticator (Partner Center > Account settings > Legal entity info > Verification Summary).
Next: once verified, add the Partner One ID under the app's Branding & properties > Publisher verification and click Verify, signed in as `admin@mailypoppins.com`.
TUM: admin approval unobtainable; revisit if the tenant policy changes.

## Blocks

- [#0036](0036-graph-sync-backend.md) Graph sync backend.
