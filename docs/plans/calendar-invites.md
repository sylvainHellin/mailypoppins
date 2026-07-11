# Design: Calendar invitations (iMIP over SMTP/IMAP)

> Status: design, nothing implemented (2026-07-11). Implementer-facing.
> Scout: [.agents/research/2026-07-11-calendar-contacts-scout.md] (contacts/iMIP touchpoints).
> Tickets: [#0020](../tickets/0020-accept-calendar-invitations.md),
> [#0027](../tickets/0027-imip-receive-parse.md),
> [#0028](../tickets/0028-imip-send-invite.md),
> [#0029](../tickets/0029-imip-rsvp-reply.md),
> [#0030](../tickets/0030-imip-reply-reconciliation.md).

## Decision summary (settled — do not reopen)

- **D1 — Transport.** v1 uses **iMIP MIME (RFC 6047 / RFC 5546) over SMTP/IMAP for every
  account, Graph and non-Graph alike.** No Microsoft Graph calendar path in v1. The TUM
  M365 tenant requires IT-admin approval for Graph API access that is currently
  unobtainable, so Graph is a *future* sync backend with its own gating ticket
  ([#0035](../tickets/0035-graph-admin-approval.md) approval →
  [#0036](../tickets/0036-graph-sync-backend.md) backend). The requirement is **universal
  recipient compatibility**: Gmail, Outlook, and Apple Mail must render `method=REQUEST`
  as an actionable invite.
- **D2 — On-disk representation.** A received invite is stored as **both** (a) new
  frontmatter fields on the email `.md` (event summary), **and** (b) the **raw `.ics`
  saved as a sidecar** next to the email (same directory as attachments). The `.ics` is
  the source of truth for `UID`/`SEQUENCE` round-tripping; frontmatter is the render/query
  cache.
- **D3 — TUI receive surface.** Invite **badge/glyph** in the email list row; an **event
  summary card** at the top of the preview pane; **RSVP via a single key** opening a small
  overlay (Accept / Tentative / Decline). Do **not** burn three top-level keys. No RSVP
  note text in v1.
- **D4 — Organizer-side tracking.** Incoming `text/calendar; method=REPLY` parts are parsed
  during sync, matched by event `UID` to the sent invite, and attendee statuses are updated
  in the local invite's frontmatter and shown in the preview card. **Honest caveat:** with
  no Graph, the user's server-side Exchange calendar is never touched — accepting via `mp`
  does not flip the event in Outlook / Mac Calendar, and sent invites create no event in the
  user's own Exchange calendar.
- **D5 — Receiving today.** `text/calendar` parts are currently silently dropped by
  `src/parse.rs` (neither body nor attachment). v1 must detect them, save the `.ics`, parse
  the `VEVENT`, and populate frontmatter.
- **D6 — Recurrence.** v1 **displays** a human-readable `RRULE` summary and can
  accept/decline the **whole series only**. Creating recurring invites and per-occurrence
  responses are v2.
- **D7 — CLI-first.** `mp send --invite --start … --end …/--duration --location --summary`
  (attendees from `--to`/`--cc`); plus `accept` / `tentative` / `decline` CLI commands. The
  TUI wraps the library functions — no subprocess spawning.
- **D8 — Cancellations/updates.** `METHOD:CANCEL` and bumped-`SEQUENCE` updates are out of
  scope for v1; noted as follow-up ([#0031](../tickets/0031-imip-cancel-update.md)).

## Architecture

Two pipelines change, plus a small store convention. All work is in the library; the CLI and
TUI call it.

### 1. Receive / parse (`src/parse.rs`)

Today `extract_body_parts` (`parse.rs:202-237`) collects only the first `text/plain` and
first `text/html`; `is_attachment_part` (`parse.rs:254-277`) keeps non-text parts with a
filename. A `text/calendar` part is `text/…` without necessarily a filename, so it falls
through both — **invites are dropped**.

Add a dedicated calendar branch that runs during part traversal:

1. **Detect** any `text/calendar` MIME part (inline or `.ics`-named attachment).
2. **Persist** the raw bytes as a sidecar `.ics` in the email's attachment directory (D2).
   Reuse the existing attachment-path scheme so archive/move keeps it colocated (mind
   [#0006](../tickets/0006-attachment-paths-after-archive.md) path handling).
3. **Parse** the `VCALENDAR`/`VEVENT` with the `icalendar` crate. Read `METHOD`, and from the
   first `VEVENT`: `UID`, `SUMMARY`, `DTSTART`/`DTEND` (or `DURATION`), `LOCATION`,
   `ORGANIZER`, `ATTENDEE` list with `PARTSTAT`, `SEQUENCE`, and `RRULE` (rendered to a
   short human string, D6).
4. **Populate frontmatter** (schema below) on the email `.md`.

`FetchedEmail` / `AttachmentData` (`parse.rs:178-181` and struct near `:327`) gain the parsed
invite payload so the writer that renders the `.md` can emit frontmatter. Keep parsing
best-effort: a malformed `.ics` still saves the sidecar and marks the email as
"invite (unparsed)" rather than failing the sync.

**Branch by `METHOD`:**
- `REQUEST` → a received invitation (render card + RSVP overlay).
- `REPLY` → an incoming RSVP; hand to the reconciliation step (D4), do **not** render as an
  invite card. See §4.
- `CANCEL` → v1 out of scope (D8); save sidecar, tag frontmatter `method: CANCEL`, no action.

### 2. Send / invite (`src/send.rs`)

Today the body is `MultiPart::alternative(TEXT_PLAIN, TEXT_HTML)` (`send.rs:670-683`), wrapped
in `MultiPart::mixed()` when attachments exist (`send.rs:685-716`). An invite needs a new MIME
shape:

```
multipart/mixed
├── multipart/alternative                     ← top-level method=REQUEST applies here
│   ├── text/plain            (human body)
│   ├── text/html             (human body, optional)
│   └── text/calendar; method=REQUEST; charset=UTF-8   (the VEVENT — the contract)
└── application/ics; name="invite.ics"        ← optional hardening (same bytes)
```

- The `text/calendar` part **inside** `multipart/alternative` is **the contract**: it is what
  Outlook and Gmail act on to render an actionable invite (per D1 universal-compatibility
  requirement and ticket 0026). Shipping this part correctly is the requirement.
- The `application/ics` attachment is **optional hardening, not a requirement.** It carries the
  same payload for long-tail cases — manual `.ics` import, clients that ignore inline parts,
  and gateway MIME mangling (Gmail itself sends both parts). The downside: some older Outlook
  versions render the attachment as a confusing duplicate of the inline invite. **Ship it in v1**
  behind this rationale, and **drop it if live testing shows duplicate rendering.**
- Build the `VEVENT` with `icalendar`, `METHOD:REQUEST`, `SEQUENCE:0` for a new invite.
  Required props: `UID` (generate + persist), `DTSTAMP`, `DTSTART`/`DTEND`-or-`DURATION`,
  `ORGANIZER` (= sending account primary address), `ATTENDEE` per `--to`/`--cc` recipient
  with `RSVP=TRUE`, `SUMMARY`. Optional: `LOCATION`, `DESCRIPTION`.
- **`ORGANIZER` must equal the sending account's primary address** or Exchange servers drop
  the invite. Validate before send; error clearly if mismatched.
- lettre can express this tree; the current fixed alternative/mixed builder needs one new
  branch for the calendar part. Recipient formatting/quoting stays as-is (`send.rs:451-545`).
- Persist the sent invite locally the same way as a received one (frontmatter + sidecar
  `.ics` in the Sent copy) so REPLY reconciliation (§4) has something to match against.

### 3. Data model

**Frontmatter** — a nested `event:` block on the email `EmailFrontmatter`
(`src/types.rs:45-61`), present only when the email carries an invite:

```yaml
event:
  uid: "abc123@mailypoppins"      # from VEVENT UID; matches REPLY/CANCEL
  method: REQUEST                 # REQUEST | REPLY | CANCEL
  sequence: 0
  summary: "LOC Day planning"
  start: 2026-07-20T14:00:00+02:00
  end: 2026-07-20T15:00:00+02:00
  location: "Room 4.12"
  organizer: "chair@tum.de"
  rsvp: needs-action              # own status: needs-action | accepted | tentative | declined
  recurrence: "Weekly on Monday"  # human RRULE summary, empty if none (D6)
  attendees:                      # per-attendee status, updated by REPLY reconciliation (D4)
    - address: "a@example.com"
      status: accepted            # needs-action | accepted | tentative | declined
    - address: "b@example.com"
      status: needs-action
```

**Sidecar `.ics`** — raw calendar bytes, colocated with the email's attachments. Source of
truth for `UID`/`SEQUENCE`. On RSVP, generate the `REPLY` from this file (bump nothing;
`SEQUENCE` echoes the request), never from the frontmatter cache.

**Multi-machine consistency.** The `event:` frontmatter (attendee statuses and own RSVP state)
is **local derived state**, reconstructible purely from IMAP-visible messages: the sent invite
in Sent, incoming attendee `METHOD:REPLY` emails, and the user's own RSVP `REPLY` in Sent. It
holds no information that does not already exist as a message on the server. Reconciliation (§4)
must therefore be **(a) idempotent** and **(b) re-runnable over the whole existing mailstore**,
not only over newly fetched messages. Consequence: two machines syncing the same IMAP account
converge on identical event state with **no direct machine-to-machine sync** — each rebuilds the
same derived state from the same messages.

### 4. Sync-time REPLY reconciliation (D4)

During sync, when parse produces a `text/calendar; method=REPLY`:

1. Extract `UID` and the replying `ATTENDEE`'s `PARTSTAT`.
2. Find the local sent invite whose `event.uid` matches (search Sent / invite index).
3. Update that invite's `attendees[].status` for the matching address, in frontmatter
   (rewrite the `event:` block in place — see lessons-learned on in-place YAML rewrites).
4. The preview card then reflects live attendee statuses.
5. The REPLY email itself is not shown as an invite card (§1 branch).

Because `event:` is local derived state (§3, multi-machine consistency), this step must be
**idempotent** (re-processing the same REPLY yields the same result) and **re-runnable over the
whole existing mailstore**, not just newly fetched messages — so a fresh checkout or a second
machine can reconstruct identical event state from IMAP alone. Wire it into sync **and** expose
a full re-scan command mirroring `mp contacts rebuild` (e.g. `mp calendar rebuild`; exact name
is the implementer's call).

**Caveat to surface in UI/docs (D4):** this updates only the *local* mirror. Without Graph,
the user's Exchange calendar is not updated — no event is created on accept, and RSVPs sent
by `mp` are not reflected in the organizer's Outlook. State the limitation plainly in the
preview card footer and in `docs/`.

### 5. CLI surface (D7)

```
mp send --invite --to <addr> [--cc <addr>] \
        --start <rfc3339> (--end <rfc3339> | --duration <dur>) \
        --summary <text> [--location <text>] [--description <text>]

mp invite accept   <email-path>
mp invite tentative <email-path>
mp invite decline  <email-path>
```

- Attendees come from `--to`/`--cc` (per ticket 0026). No dedicated attendee flag in v1.
- The accept/tentative/decline commands read the sidecar `.ics`, build a `method=REPLY`
  VEVENT with the account's `ATTENDEE` `PARTSTAT` set, email it to the `ORGANIZER`, and
  update local `event.rsvp`.
- TUI RSVP overlay (D3) calls the exact same library functions — no subprocess.

### 6. TUI surface (D3)

- **List row:** an invite badge/glyph column when `event:` is present (distinct from the
  attachment paperclip). Style via the semantic `Theme`.
- **Preview card:** a bordered card at the top of the preview pane — title, time range,
  location, organizer, own RSVP state, and per-attendee statuses (from D4). Card footer notes
  the "not synced to Exchange" caveat when the account is Exchange/Graph.
- **RSVP:** one key (e.g. `V` for "invite", avoiding the taken `a`=archive) opens a small
  three-choice overlay (Accept / Tentative / Decline). Reuse the confirm/choice overlay
  primitive. No note text field in v1.
- Register the key + overlay through the keymap-as-data table once the TUI-foundation package
  ([#0032](../tickets/0032-tui-foundation-package.md)) lands; until then follow the existing
  `keys.rs` + `help_sections()` + website triple-update rule.

## Dependency choice

**`icalendar` crate.** Rationale, kept brief:
- Handles both **parse and build** of `VCALENDAR`/`VEVENT`, so one dep covers receive (§1)
  and send (§2).
- Already named as the candidate in tickets 0020/0026 and both scout briefs.
- Pure-Rust, no C deps — fits the offline `cargo test` budget and cross-platform goals.
- If per-property control over `METHOD`/`SEQUENCE`/`PARTSTAT` at the `VCALENDAR` level is
  awkward through the builder, we may hand-assemble a few header lines around
  `icalendar`-serialized components; acceptable and localized. Confirm during implementation
  (see open questions).

## v1 / v2 boundary

| Capability | v1 | v2 / future |
|---|---|---|
| Receive `REQUEST`, save `.ics`, parse, frontmatter | ✅ | |
| List badge + preview card + RSVP overlay | ✅ | |
| Send `REQUEST` invite (CLI + TUI) | ✅ | |
| Send `REPLY` (accept/tentative/decline) | ✅ | |
| Sync-time `REPLY` reconciliation into local invite | ✅ | |
| Recurrence display (human `RRULE`) | ✅ | |
| Accept/decline **whole series** | ✅ | |
| Per-occurrence responses; create recurring invites | | ✅ ([#0031] scope-adjacent) |
| `METHOD:CANCEL` / `SEQUENCE`-bump updates | | ✅ [#0031](../tickets/0031-imip-cancel-update.md) |
| RSVP note text | | ✅ |
| Server-side Exchange calendar sync | | ✅ Graph backend [#0036](../tickets/0036-graph-sync-backend.md) (gated on [#0035](../tickets/0035-graph-admin-approval.md)) |

## Risks

- **Exchange `ORGANIZER` matching (Graph-future territory).** Exchange silently drops invites
  whose `ORGANIZER` ≠ the authenticated sender, and RSVP mapping back to the event `UID` is
  fragile across some servers. Because v1 is iMIP-only and does **not** write the Exchange
  calendar, these belong to the **Graph-future** work: validated organizer identity and
  native accept/decline land with the Graph sync backend
  ([#0036](../tickets/0036-graph-sync-backend.md)). v1 mitigates by hard-validating
  `ORGANIZER == account primary address` before send and by surfacing the "not synced" caveat.
- **Silent-drop regression risk.** The parse change touches the hot part-traversal path; a bug
  could drop real attachments or bodies. Cover with fixtures for inline `REQUEST`, `.ics`
  attachment, `REPLY`, malformed `.ics`, and a plain multipart email (no calendar).
- **In-place frontmatter rewrite** for attendee-status updates (D4) reuses the pattern that
  bit us before — see `docs/lessons-learned.md` on YAML round-tripping; rewrite only the
  `event:` block, preserve the body.
- **UID collisions / round-tripping.** Always source `UID`/`SEQUENCE` from the sidecar `.ics`,
  never regenerate on reply.

## Open implementation questions

1. `icalendar` builder ergonomics for top-level `METHOD` and `VCALENDAR`-level `SEQUENCE`
   echo on REPLY — confirm whether a thin header-assembly wrapper is needed (dependency
   section). *Resolve in implementation; does not block the design.*
2. Sidecar `.ics` filename/dir convention vs. the existing attachment scheme
   ([#0006](../tickets/0006-attachment-paths-after-archive.md)) — pick one that survives
   archive/move. *Implementer's call within the store convention; not a product decision.*
3. Final RSVP keybinding letter (`V` proposed) — settle when the keymap table
   ([#0032](../tickets/0032-tui-foundation-package.md)) lands to avoid collisions.
