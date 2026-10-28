# Design: server-side calendar

> Status: design, nothing implemented (2026-10-28). Implementer-facing.
> Scoping: `.sync/calendar-scoping/index.html` and Sylvain's comment export of 2026-10-28.
> Decisions: `decision-log.md`, entries of 2026-10-28.
> Issues: PERSO-200 (phase 1), PERSO-255 (phase 2), PERSO-256 (phase 3), PERSO-257 (phase 4), PERSO-254 (CalDAV provider choice, Sylvain's task).
> Predecessor: [calendar-invites.md](calendar-invites.md), the iMIP path, which stays for accounts without a calendar backend.

mp gets an Outlook-like calendar: it shows the events of every account, answers invitations, creates events with attendees, and updates or cancels them.
The daemon owns the calendar engine, and the CLI, the TUI and the desktop app offer all of it through the same `calendar.*` and `event.*` calls ([architecture.md](../architecture.md), "Project invariants").
Each account reaches its server calendar through one backend: Microsoft Graph, Exchange Web Services (EWS) or CalDAV.

## Decision summary

These are settled; the decision log carries the why and the rejected options.

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
- D10, the TUI gets a full week grid.
  The TUI shows agenda, day and week views like the desktop app, with calcure (<https://github.com/anufrievroman/calcure>, a Python terminal calendar) as the layout reference.
- D11, sync by polling.
  Webhooks and push subscriptions need a public endpoint, which a local daemon does not have.
- D12, Hines stays out.
  Its tenant blocks third-party apps that need admin consent until its IT approves, and publisher verification (PERSO-163) does not lift a tenant block.

## Current state

- The iMIP path of [calendar-invites.md](calendar-invites.md) is built: the daemon parses the `.ics` of a mail and keeps it as a sidecar.
- `mp invite accept|tentative|decline` sends a `METHOD:REPLY` mail, and `mp send --invite` sends a `REQUEST` for a timed or all-day event, with updates through `--uid` and `--sequence`; `CANCEL` and `SEQUENCE` are handled.
- Attendee status is folded from `REPLY` mails.
- The agenda (`calendar.events`) is derived from invitation mails only.
  An event created in Outlook or on the phone, or accepted elsewhere without a mail, never appears.
- A recurring event shows as a text summary, with no expanded occurrences.
- The TUI has a calendar view and an RSVP overlay; the desktop app has a Calendar view, an invite card and a "New invitation" form.
- Graph accounts are refused for RSVP and for sending invitations.
- `GRAPH_SCOPES` (`crates/mp-core/src/oauth2.rs`) holds `Mail.Read`, `Mail.ReadWrite`, `Mail.Send` and `offline_access`, with no calendar scope, and the token code does not detect a missing scope.
- The daemon serves `calendar.events`, `calendar.rsvp`, `calendar.rebuild` and `send.invite`.
- cal-cli (`~/code/side-projects/cal-cli`, Python) reads and writes the TUM calendar through EWS (exchangelib, password) and Gmail through the Google Calendar API, with free/busy and configured rooms.

## Accounts and backends

- EVOQS, Microsoft 365: Graph for mail and calendar; Sylvain administers the tenant and consents to `Calendars.ReadWrite` himself.
- TUM, LRZ on-prem Exchange (`xmail.mwn.de`): IMAP and SMTP for mail, EWS for the calendar with the same password; cal-cli proves that password EWS works there.
- Personal: a CalDAV provider with RFC 6638 scheduling once Sylvain has moved off Google (PERSO-254).
- Hines, Microsoft 365: no calendar backend until its IT grants admin consent (D12).
- Proton and OVH: no calendar API; iMIP in the mail view keeps working.

## Architecture

### Account setting

Each account gains a calendar backend setting next to its mail settings, provisional name `calendar_backend`, with the values `graph`, `ews`, `caldav` and `none`.
An account with `auth_method = "graph"` defaults to `graph`; every other account defaults to `none`.
`ews` reuses the account's stored password and takes an endpoint (`https://xmail.mwn.de/EWS/Exchange.asmx` for TUM, or autodiscovery); `caldav` takes a server URL and its own credential in the secrets store when it differs from the mail one.
The setup wizards and `mp config` ask for it in all three clients.

### Neutral event model

An event carries its account, calendar, server id, iCalendar `UID`, `RECURRENCE-ID` for an occurrence, summary, start and end with time zone, an all-day flag, location, body, organizer, attendees with role and status, the user's own response, the recurrence rule of a series master, reminder, online-meeting link, sensitivity and show-as.
Times are stored as UTC instants plus the IANA zone of the event, so a view renders in the user's zone and an edit keeps the organizer's.
All-day events keep a date range with an exclusive end, as in iCalendar; the Graph and EWS adapters convert their midnight-to-midnight form.

### Event store

A per-account table set in the existing sqlite store, holding calendars (name, colour, visibility, permissions) and the expanded occurrences of the rolling window, plus each backend's sync token.
Visibility per calendar is a local preference and survives a cache rebuild.
`calendar.rebuild` drops the cache of an account and resyncs it from the server; on an iMIP account it keeps its current meaning, refolding the invitation mails.

### Sync

- Polling on the daemon's tick, plus an immediate refresh after every write.
- Graph: `calendarView/delta` per calendar over the window, with a full resync on HTTP 410.
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
Graph accepts a `Prefer: outlook.timezone` header, which fixes the zone of the times it returns, but the event's own zone fields still come as Windows names.

### Credentials and scopes

`GRAPH_SCOPES` gains `Calendars.ReadWrite`.
The daemon records the scopes a token was granted, and a Graph account whose token lacks the calendar scope gets a re-login prompt in every client instead of a failing call.
EWS reuses the IMAP password; nothing new is stored for TUM.

## Backend notes

### Graph

- A hand-rolled client on reqwest and serde, about a dozen endpoints; the auth already exists.
- Read: `GET /me/calendars`, `GET /me/calendars/{id}/calendarView/delta`.
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

Method names are provisional; `calendar.events`, `calendar.rsvp` and `send.invite` fold into them, and `docs/daemon-protocol.md` gets the final shapes when phase 1 lands.

- `calendar.list {account?}`: calendars per account, with colour, visibility and permissions.
- `calendar.set_visibility {calendar, visible}`: shows or hides a calendar.
- `calendar.view {range, calendars?}`: the expanded occurrences in a range, merged across accounts.
- `event.get {event}`: one event with its attendees and their status.
- `event.respond {event, response, comment?, send_response, proposed_time?}`: accept, tentative or decline, optionally proposing a new time.
- `event.create {calendar, fields, attendees, recurrence?, online_meeting?, reminder?}`: a new event; with attendees the server sends the invitations.
- `event.update {event, scope, fields}` and `event.cancel {event, scope, comment?}`: the organizer's operations, with `scope` naming the occurrence or the series.
- `calendar.freebusy {addresses, range}`: availability of attendees and rooms when picking a time.
- `calendar.rebuild {account}`: drops and resyncs an account's cache.
- Reminders reach the clients as daemon events, and the desktop notification follows the existing notification path.

Every write is an `operation.*` with the existing journal, so an interrupted create or respond reports where it stopped.

## Client surfaces

- CLI: provisional `mp calendar list|view|show`, `mp event respond|create|update|cancel` and `mp calendar freebusy`, with `--json` for agents; `mp invite` and `mp send --invite` keep working on iMIP accounts.
- TUI: agenda, day and week views on the calcure layout model, an event card, a respond overlay that adds the comment, the send-response choice and the proposed time to today's RSVP overlay, and an event form for create and update.
- Desktop: the Calendar view gains day and week grids beside the agenda, and the "New invitation" form becomes the event form.
- Every new capability gets its row in [parity-matrix.md](../parity-matrix.md) with its CLI, TUI and desktop location, and the website pages follow the CLI.

## Phases

### Phase 1, Graph read, views and server-side responses (PERSO-200)

- Event store as a disposable cache of the rolling window, rebuilt from the server.
- `CalendarBackend` trait over the neutral event model, and the per-account `calendar_backend` setting.
- `calendar.list` and `calendar.view` over Graph.
- `Calendars.ReadWrite` in `GRAPH_SCOPES`, with a re-login prompt when a stored token lacks it.
- Agenda, day and week views in the CLI, the TUI and the desktop app, with calendars shown or hidden per account.
- The invite card on a Graph mail wired to its server event (`eventMessage` to `event`), with accept, tentative and decline, an optional comment and the send-response option.
- Done when the EVOQS account shows the events created in Outlook in all three clients, and an answer from mp shows in Outlook.

### Phase 2, Graph write (PERSO-255, after phase 1)

- Create, update and cancel with attendees, for single and simple recurring events.
- Edit or cancel a single occurrence of a series, on immutable ids.
- Propose a new time.
- Teams link toggle, attendee status, free/busy through `getSchedule` when picking a time.
- Reminders as desktop notifications.

### Phase 3, EWS for on-prem Exchange, TUM (PERSO-256, after phase 1)

- Spike first: probe EWS auth on `xmail.mwn.de` (Basic or NTLM), and check the calendar coverage of the `ews` crate against hand-rolled SOAP on quick-xml; the spike's outcome picks the client.
- Read, respond with comment, send-response option and proposed time, create, update and cancel, single occurrences.
- Free/busy through `GetUserAvailability`, rooms by inviting the room mailbox, TUM delegate calendars for room booking.
- cal-cli's TUM part retires once mp covers it.

### Phase 4, CalDAV (PERSO-257, after phase 1 and after PERSO-254)

- Read with `sync-collection` and client-side recurrence expansion.
- Write with RFC 6638 server scheduling, and iMIP when the server does not advertise it.
- Tested against the provider Sylvain chooses.
- cal-cli retires fully.

### Later

- Shared and delegated calendars beyond TUM rooms.
- A room finder (`GetRoomLists` and `GetRooms`, Graph `findRooms`).
- A month view.
- ICS subscription feeds.
- Server contacts as an additional contact source: Graph `Contacts.Read` and `/me/contacts` (the contacts part of PERSO-200 before its reshape), EWS contacts for TUM, CardDAV for the personal account.
- Hines, once its IT grants admin consent.

## Dependencies

Candidates, each needing Sylvain's approval when its phase starts:

- libdav, and calcard or icalendar, for CalDAV and iCalendar.
- rrule, for client-side recurrence expansion.
- ews (Thunderbird) or quick-xml, for EWS.
- A Windows-to-IANA zone table from CLDR, as a crate or as a generated file.

Graph needs nothing new beyond reqwest and serde.

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
- Webhooks and push subscriptions (D11).
- Calendars on Proton, which has no calendar API.
- Everything under "Later" until it is scheduled.
