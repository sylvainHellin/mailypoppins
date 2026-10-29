# Design: server-side calendar

> Status: phase 1 built (2026-10-29, branch `calendar-phase-1`); phases 2 to 4 are design. Implementer-facing.
> Built: [architecture.md](../architecture.md), "Server calendar", and [daemon-protocol.md](../daemon-protocol.md), "The server calendar", describe what phase 1 shipped.
> Scoping: `.sync/calendar-scoping/index.html` and Sylvain's comment export of 2026-10-28.
> Decisions: `decision-log.md`, entries of 2026-10-28 (the plan) and 2026-10-29 (phase 1).
> Issues: PERSO-200 (phase 1), PERSO-255 (phase 2), PERSO-256 (phase 3), PERSO-257 (phase 4), PERSO-254 (CalDAV provider choice, Sylvain's task).
> Predecessor: [calendar-invites.md](calendar-invites.md), the iMIP path, which stays for accounts without a calendar backend.

mp gets an Outlook-like calendar: it shows the events of every account, answers invitations, creates events with attendees, and updates or cancels them.
The daemon owns the calendar engine, and the CLI, the TUI and the desktop app offer all of it through the same `calendar.*` and `event.*` calls ([architecture.md](../architecture.md), "Project invariants").
Each account reaches its server calendar through one backend: Microsoft Graph, Exchange Web Services (EWS) or CalDAV.

## Decision summary

Sylvain settled D1 to D5, D9 and D10, and the decision log carries their why and their rejected options.
The other entries are design choices of this plan; phase 1 built D6, D7, D8 and D11 as written, and the decision log's entries of 2026-10-29 carry the choices phase 1 made on top of them.

- D1, backend order.
  Graph comes first, then EWS for TUM, then CalDAV.
  Graph is the cheapest API to build the engine, the store and the views against, TUM carries most meetings and room bookings, and the personal calendar waits on its move to a CalDAV provider.
- D2, the calendar backend is a per-account setting, independent of the mail transport.
  TUM mail stays on IMAP and SMTP, which LRZ supports and which works; only its calendar, and later its contacts, go through EWS on the same account and the same credentials.
  A Graph account uses Graph for mail and calendar.
  If LRZ moves TUM to Exchange Online, the account switches to Graph for everything.
- D3, EWS for on-prem Exchange accounts.
  TUM runs on-prem Exchange at LRZ (`xmail.mwn.de`), which Graph cannot reach and which exposes no CalDAV.
  The decision log entry of 2026-10-21 rejected EWS as the transport of the EVOQS Microsoft 365 account only, because Microsoft retires EWS in Exchange Online, and it stands.
- D4, generic CalDAV only.
  The personal calendar moves from Google to a CalDAV provider with server-side scheduling (RFC 6638); mp builds no Google Calendar API backend and no Google-specific CalDAV path.
  Choosing the provider and migrating the events and the address book is Sylvain's task (PERSO-254).
- D5, mp absorbs cal-cli.
  Free/busy, rooms by inviting the room mailbox, and creating and answering events move into mp.
  cal-cli's TUM part retires with phase 3, and cal-cli retires fully with phase 4; agents then use mp, with one tool and one credential store.
- D6, the event store is a disposable cache.
  It holds a rolling window per account, one month back and twelve months ahead to start with, and is rebuilt from the server whenever needed.
  The server is the source of truth, so a lost or corrupt cache costs one resync and no data.
- D7, one engine over a neutral event model.
  A `CalendarBackend` trait is implemented per backend, and the engine merges the accounts, deduplicates and serves the clients without knowing which backend an event came from.
- D8, answers go through the server on an account with a calendar backend.
  An invitation mail on such an account links to its server event, and accept, tentative and decline call the server API, which updates the calendar and sends the reply.
  Accounts without a calendar backend (Proton, OVH) keep the iMIP path of calendar-invites.md unchanged.
- D9, the first-release cut.
  In: agenda, day and week views; calendars shown or hidden per account; respond (accept, tentative, decline) with an optional comment and a "do not send a response" option; propose a new time; create, update and cancel single and simple recurring events with attendees; edit a single occurrence of a series; attendee status; a Teams link toggle on Microsoft 365; free/busy when picking a time; reminders as desktop notifications.
  Out: shared calendars beyond TUM rooms, a room finder, a month view.
  The first release is phases 1 and 2 on Graph accounts.
  EWS reaches the same cut in phase 3 and CalDAV in phase 4; on CalDAV, proposing a new time (an iTIP `COUNTER`) ships only if the chosen server supports it.
- D10, the TUI and the desktop app both get a week grid.
  Both show agenda, day and week views, the week grid built in phase 1 in both clients.
  calcure (<https://github.com/anufrievroman/calcure>, a Python terminal calendar) is the layout reference for the TUI.
- D11, sync by polling on every backend to start with.
  Graph webhooks need a public HTTPS endpoint for Microsoft to call, which a local daemon does not have.
  EWS streaming and pull notifications run over outbound connections and would work for TUM, but polling keeps one sync path across the three backends while the engine is built; EWS notifications are listed under "Later".
- D12, Hines stays out.
  Its tenant blocks third-party apps that need admin consent until its IT approves, and publisher verification (PERSO-163) does not lift a tenant block.

## Current state

After phase 1:

- A Graph account reads its server calendar: the daemon caches the window of every calendar and syncs it after the mail pass of each tick, with expanded occurrences of recurring events.
- Every account still has the iMIP path of [calendar-invites.md](calendar-invites.md); its invitation mails show as the virtual `imip` calendar, and an invitation the server calendar also holds shows once, as the server's event.
- The daemon serves `calendar.list`, `calendar.set_visibility`, `calendar.view`, `event.get` and `event.respond`, publishes `calendar.changed`, and `calendar.rebuild` also resyncs the server cache; `calendar.events`, `calendar.rsvp` and `send.invite` stay as they were.
- The CLI has `mp calendar list|view|show|hide|rebuild` and `mp event show|respond`, and `mp invite` answers through `event.respond`.
- The TUI and the desktop app show agenda, day and week layouts with an event card, a calendars panel and a respond dialog with a comment and the send-response choice.
- `GRAPH_SCOPES` holds `Calendars.ReadWrite`; a token without it gets a re-login prompt in every client, and mail keeps working.
- Graph accounts are still refused for iMIP answers and for sending invitations; creating events waits for phase 2.
- cal-cli (`~/code/side-projects/cal-cli`, Python) reads and writes the TUM calendar through EWS (exchangelib, password) and Gmail through the Google Calendar API, with free/busy and configured rooms.

## Accounts and backends

- EVOQS, Microsoft 365: Graph for mail and calendar; Sylvain administers the tenant and consents to `Calendars.ReadWrite` himself.
- TUM, LRZ on-prem Exchange (`xmail.mwn.de`): IMAP and SMTP for mail, EWS for the calendar with the same password; cal-cli proves that password EWS works there.
- Personal: a CalDAV provider with RFC 6638 scheduling once Sylvain has moved off Google (PERSO-254).
- Hines, Microsoft 365: no calendar backend until its IT grants admin consent (D12).
- Proton and OVH: no calendar API; iMIP in the mail view keeps working.

## Architecture

### Account setting

Each account has a calendar backend setting next to its mail settings, `calendar_backend`, with the values `graph`, `ews`, `caldav` and `none`.
An account with `auth_method = "graph"` defaults to `graph`; every other account defaults to `none`.
Phase 1 reads the key and shows the resolved value in `mp config show`; `ews` and `caldav` are refused as not supported yet and read as no backend.
`ews` will reuse the account's stored password and take an endpoint (`https://xmail.mwn.de/EWS/Exchange.asmx` for TUM, or autodiscovery); `caldav` will take a server URL and its own credential in the secrets store when it differs from the mail one.
The setup wizards ask for it once a second backend exists, in phase 3; with Graph alone the default is the only useful value.

### Neutral event model

An event carries its account, calendar, server id, iCalendar `UID`, `RECURRENCE-ID` for an occurrence, summary, start and end with time zone, an all-day flag, location, body, organizer, attendees with role and status, the user's own response, the recurrence rule of a series master, reminder, online-meeting link, sensitivity and show-as.
Times are stored as UTC instants plus the IANA zone of the event, so a view renders in the user's zone and an edit keeps the organizer's.
All-day events keep a date range with an exclusive end, as in iCalendar; the Graph and EWS adapters convert their midnight-to-midnight form.

### Event store

Each account has six tables in its sqlite store (`src/store/calendar.rs`): the calendars (name, colour, permissions), the visibility preferences, the expanded occurrences of the rolling window, each calendar's sync token with the window it was minted for, what a backend resolved about a series, and the server event each invitation mail stands for.
Visibility per calendar is a local preference in a table of its own, which a cache drop leaves alone and a store rebuild carries over.
`calendar.rebuild` drops the cache of an account and resyncs it from the server; on every account it keeps its iMIP meaning, refolding the invitation mails.

### Sync

- Polling after the mail pass of the daemon's tick, plus an immediate sync after every answer, both under one lock per account.
- Graph: `calendarView/delta` over the window, with a full resync on HTTP 410, `syncStateNotFound` or `resyncRequired`.
  A delta token is bound to the start and end of the window its first request named, so the daemon passes a token only for the window it was minted for and re-baselines daily, with a fresh full delta over the moved window.
- `calendar.changed` goes out only when a sync moved the cached rows.
- EWS: `SyncFolderItems` on each calendar folder, then `FindItem` with a `CalendarView` over the window to get the expanded occurrences.
- CalDAV: `sync-collection` (RFC 6578) where the server supports it, else a ctag check and an etag comparison.
- Graph and EWS expand recurring events on the server; CalDAV needs client-side `RRULE` expansion.

### Invitations on an account with a backend

- Graph: an invitation arrives as an `eventMessage`, whose `event` navigation property is the server event.
- EWS: a `MeetingRequest` names its event through `AssociatedCalendarItemId`.
- CalDAV with RFC 6638: the server files the invitation into the calendar (or the schedule inbox) and the mail carries the same `UID`.
- The invite card on such a mail shows the server event and answers through `event.respond`; the iMIP-derived row is not shown beside the server event.
- Events are deduplicated across sources on `UID` plus `RECURRENCE-ID`.

### Time zones

Graph and EWS name Windows time zones (`W. Europe Standard Time`), so the engine carries the CLDR Windows-to-IANA table.
The table is generated from CLDR's `windowsZones.xml` into `crates/mp-core/src/windows_zones.rs` by `scripts/gen-windows-zones.py`, with no new dependency.
The Graph backend sends no `Prefer: outlook.timezone`, so times come back in UTC, and reads the event's own zone from `originalStartTimeZone`; a zone the table lacks (`tzone://Microsoft/Custom`) is labelled UTC with the instants exact.
An all-day event's `RECURRENCE-ID` is keyed on its date both from a mail and from the server, so the two sources dedup.

### Credentials and scopes

`GRAPH_SCOPES` holds `Calendars.ReadWrite`, and an account asks for it only when its calendar backend is Graph.
The token cache records the scopes Entra granted, and a refresh whose scopes are refused is retried once without the calendar scope, so mail survives a tenant that has not consented.
A Graph calendar account whose token lacks the scope reports it through the existing `account.credentials` (`missing_scopes`) and a `calendar:<name>` health check, which every client shows as the re-login prompt `calendar needs a re-login (mp config oauth2-login -A <name>)`; the calendar calls refuse with the same prompt instead of failing at the server.
EWS reuses the IMAP password; nothing new is stored for TUM.

## Backend notes

### Graph

- A hand-rolled client on reqwest and serde, about a dozen endpoints; the auth already exists.
- Read: `GET /me/calendars`, then `GET /me/calendars/{id}/calendarView/delta` per calendar, which v1.0 routes but documents only for the default calendar.
  When it answers 400 or 404 the default calendar falls back to `GET /me/calendarView/delta` and any other calendar to a plain `GET /me/calendars/{id}/calendarView` read in full each sync, the fallback remembered until the next baseline.
  A calendar whose ACL refuses the read fails on its own and the others still sync.
- Series: Graph gives each occurrence its own `iCalUId`, so an occurrence's `UID` is read once per series from its master (`GET /me/events/{seriesMasterId}`) and cached.
- Delta tokens: each token is bound to the window of its first request, so the scheduled re-baseline of the rolling window (see "Sync") starts a new delta round.
- Ids: every request sends `Prefer: IdType="ImmutableId"`, so an occurrence or an event moved between folders keeps its id.
- Respond: `POST /me/events/{id}/accept|tentativelyAccept|decline` with `comment` and `sendResponse`; `tentativelyAccept` and `decline` take `proposedNewTime` for proposing a new time.
- Write: `POST /me/calendars/{id}/events` (the server sends the invitations), `PATCH /me/events/{id}`, `POST /me/events/{id}/cancel` with a comment for the organizer, `DELETE` for an event without attendees.
- Single occurrence: `GET /me/events/{masterId}/instances` over a range, then `PATCH` or `cancel` on the instance id.
- Teams link: `isOnlineMeeting` with `onlineMeetingProvider: teamsForBusiness`.
- Free/busy: `POST /me/calendar/getSchedule`.
- Reminders: `isReminderOn` and `reminderMinutesBeforeStart`, fired by the daemon as desktop notifications.
- Invitation mail: `GET /me/messages/{id}` expanded to `microsoft.graph.eventMessage/event`.

### EWS

- The spike decides between hand-rolled SOAP on quick-xml and Thunderbird's `ews` crate (0.1.x, MPL-2.0, calendar coverage unverified), and probes the auth `xmail.mwn.de` accepts (Basic or NTLM).
- Read: `FindFolder` for the calendars, `SyncFolderItems`, `FindItem` with `CalendarView`, `GetItem` for attendees and body.
- Respond: `AcceptItem`, `TentativelyAcceptItem`, `DeclineItem` with a body and `MessageDisposition` (`SendAndSaveCopy` or `SaveOnly` for "do not send a response"); proposing a new time uses `ProposedStart` and `ProposedEnd` where LRZ's schema version has them, which the spike checks.
  The spike also checks whether `SaveOnly` leaves a response item in Drafts, which mp would then have to delete.
- Write: `CreateItem` with `SendMeetingInvitations`, `UpdateItem` with `SendMeetingInvitationsOrCancellations`, `CancelCalendarItem`.
- Single occurrence: `OccurrenceItemId` (series master plus instance index) or the occurrence's own `ItemId` from the calendar view.
- Free/busy: `GetUserAvailability`.
- Rooms: a room is booked by inviting its mailbox as a resource attendee, with its free/busy checked first, as cal-cli does; rooms come from configuration, since TUM publishes no room lists.
- Delegate calendars: TUM rooms whose calendars Sylvain holds as a delegate are read and written through a `DistinguishedFolderId` with the room's `Mailbox`.

### CalDAV

- Candidate crates: libdav 0.11 with calcard, or icalendar with rrule.
- Read: calendar-home discovery (RFC 4791, RFC 6764), `sync-collection`, and client-side expansion of `RRULE`, `RDATE`, `EXDATE` and overrides.
- Write: `PUT` of the `VCALENDAR` with `If-Match` on the etag.
- Scheduling: with `calendar-auto-schedule` advertised the server sends the invitations, replies and cancellations (RFC 6638); without it mp sends them as iMIP mails through the account's SMTP, reusing calendar-invites.md.
- Respond: the attendee's `PARTSTAT` is written to the event, and the server or iMIP sends the reply.
- Single occurrence: an override component with its `RECURRENCE-ID`.
- Propose a new time: an iTIP `COUNTER`, which fewer servers and clients honour; tests against the chosen provider decide whether it ships on CalDAV.
- Free/busy: a `VFREEBUSY` request to the schedule outbox.

## Protocol sketch

Phase 1's methods are final and [daemon-protocol.md](../daemon-protocol.md), "The server calendar", holds their shapes: `calendar.list {account?}`, `calendar.set_visibility {account, calendar, visible}`, `calendar.view {start, end, accounts?, calendars?, include_hidden?, zone?}`, `event.get {account, event}`, `event.respond {account, event, response, comment?, send_response?}` and the `calendar.changed {account}` event; `calendar.rebuild {account}` also drops and resyncs an account's server cache.
An invitation mail's event has the id `imip:<row_id>`, and `event.respond` answers it with the iMIP reply `calendar.rsvp` sends; `calendar.events`, `calendar.rsvp` and `send.invite` stay until the later phases fold them in.
The later phases' methods are provisional:

- `event.respond` gains `proposed_time` for proposing a new time (phase 2).
- `event.create {calendar, fields, attendees, recurrence?, online_meeting?, reminder?}`: a new event; with attendees the server sends the invitations.
- `event.update {event, scope, fields}` and `event.cancel {event, scope, comment?}`: the organizer's operations, with `scope` naming the occurrence or the series.
- `calendar.freebusy {addresses, range}`: availability of attendees and rooms when picking a time.
- Reminders reach the clients as daemon events, and the desktop notification follows the existing notification path.

Every write is an `operation.*` with the existing journal, so an interrupted create or respond reports where it stopped.

## Client surfaces

- CLI: phase 1 ships `mp calendar list|view|show|hide|rebuild` and `mp event show|respond`, with `--json` for agents, and `mp invite` answers through `event.respond`; `mp event create|update|cancel` and `mp calendar freebusy` are provisional names for phase 2, and `mp send --invite` keeps working on iMIP accounts.
- TUI: phase 1 ships the agenda, day and week layouts (the week falling back to the day grid on a narrow terminal), the event card, the calendars overlay, the re-login banner and a respond overlay with the comment and the send-response choice; the proposed time and an event form for create and update come in phase 2.
- Desktop: phase 1 ships the same layouts, the calendars panel, the event card with its join link and invitation mail, and answers with a comment and the send-response choice from the card and the reader; the "New invitation" form becomes the event form in phase 2.
- Every new capability gets its row in [parity-matrix.md](../parity-matrix.md) with its CLI, TUI and desktop location, and the website pages follow the CLI.

## Phases

### Phase 1, Graph read, views and server-side responses (PERSO-200), built

- The per-calendar `/me/calendars/{id}/calendarView/delta` is tried first, with the fallbacks of "Backend notes", Graph, since the route is undocumented beyond the default calendar; which path the EVOQS tenant takes is recorded per calendar in the store.
- Event store as a disposable cache of the rolling window, rebuilt from the server.
- `CalendarBackend` trait over the neutral event model, and the per-account `calendar_backend` setting.
- `calendar.list` and `calendar.view` over Graph.
- `Calendars.ReadWrite` in `GRAPH_SCOPES`, with a re-login prompt when a stored token lacks it.
- Agenda, day and week views in the CLI, the TUI and the desktop app, with calendars shown or hidden per account; the week grid is new in both the TUI and the desktop app.
- The invite card on a Graph mail wired to its server event (`eventMessage` to `event`), with accept, tentative and decline, an optional comment and the send-response option.
- Done when the EVOQS account shows the events created in Outlook in all three clients, and an answer from mp shows in Outlook.

### Phase 2, Graph write (PERSO-255, after phase 1)

- Create, update and cancel with attendees, for single and simple recurring events.
- Edit or cancel a single occurrence of a series, on immutable ids.
- Propose a new time.
- Teams link toggle, attendee status, free/busy through `getSchedule` when picking a time.
- Reminders as desktop notifications.

### Phase 3, EWS for on-prem Exchange, TUM (PERSO-256, after phase 1)

- Spike first: probe EWS auth on `xmail.mwn.de` (Basic or NTLM), check the calendar coverage of the `ews` crate against hand-rolled SOAP on quick-xml, and check whether `AcceptItem` with `SaveOnly` leaves a response item in Drafts; the spike's outcome picks the client.
- Read, respond with comment, send-response option and proposed time, create, update and cancel, single occurrences.
- Free/busy through `GetUserAvailability`, rooms by inviting the room mailbox, TUM delegate calendars for room booking.
- TUM reaches the first-release cut of D9; the Teams link toggle applies to Microsoft 365 only.
- cal-cli's TUM part retires once mp covers it.

### Phase 4, CalDAV (PERSO-257, after phase 1 and after PERSO-254)

- Read with `sync-collection` and client-side recurrence expansion.
- Write with RFC 6638 server scheduling, and iMIP when the server does not advertise it.
- The personal calendar reaches the first-release cut of D9, with proposing a new time (iTIP `COUNTER`) only if the provider supports it.
- Tested against the provider Sylvain chooses.
- cal-cli retires fully.

### Later

- Shared and delegated calendars beyond TUM rooms.
- A room finder (`GetRoomLists` and `GetRooms`, Graph `findRooms`).
- A month view.
- ICS subscription feeds.
- EWS streaming notifications for TUM in place of polling (D11).
- Server contacts as an additional contact source: Graph `Contacts.Read` and `/me/contacts` (the contacts part of PERSO-200 before its reshape), EWS contacts for TUM, CardDAV for the personal account.
- Hines, once its IT grants admin consent.

## Dependencies

Candidates, each needing Sylvain's approval when its phase starts:

- libdav, and calcard or icalendar, for CalDAV and iCalendar.
- rrule, for client-side recurrence expansion.
- ews (Thunderbird) or quick-xml, for EWS.

Phase 1 needed nothing new: Graph runs on reqwest and serde, and the Windows-to-IANA table is a file generated from CLDR.

## Risks

- Recurrence and time zone edge cases: an all-day event off by one day, and exception ids that change (Graph immutable ids avoid the latter).
- Duplicate events when iMIP and the server deliver the same invitation; the `UID` plus `RECURRENCE-ID` dedup has to cover it.
- Attendee self-identification across aliases, which decides whether the user is the organizer or an attendee.
- LRZ moving TUM to Exchange Online, which retires EWS for it; D2 keeps the switch to Graph a setting change.
- EWS auth on `xmail.mwn.de` may need NTLM, which the spike settles before any build.
- The week grid in the TUI is the costliest view to build and to keep in golden frames.
- CLI, TUI and desktop parity for every new capability ([parity-matrix.md](../parity-matrix.md)).
- Tokens issued before phase 1 lack the calendar scope; the re-login prompt has to reach every client.

## Out of scope

- A Google Calendar API backend and Google-specific CalDAV (D4).
- Graph webhooks, which need a public endpoint (D11).
- Calendars on Proton, which has no calendar API.
- Everything under "Later" until it is scheduled.
