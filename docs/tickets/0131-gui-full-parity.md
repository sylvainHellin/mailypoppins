---
id: 0131
title: M2 to M4 of the native GUI, full parity with the TUI
type: feature
priority: next
status: open
created: 2026-09-25
---

Third ticket of the [native GUI plan](../plans/native-gui.md), milestones "M2: mutations with the undo hold", "M3: compose through the external editor" and "M4: calendar, contacts, signatures and config", plus the read slices that land in M1 beside #0129.
Blocked on #0129 (M1), and #0130 (M5) is blocked on this ticket, because the milestones run in order.

## Work

- Implement every `GUI parity` row of [docs/parity-matrix.md](../parity-matrix.md) in the plan's milestones M1 (the read slices), M2, M3 and M4, each backed by the same daemon methods the TUI uses and naming the identifiers it closes.
- Replace each row's planned `GUI location`, which names its milestone today, with the surface and interaction that shipped.
- Add GUI interaction tests and cross-client scenarios as slices land, and keep TUI parity green.
- Update help and the website alongside any changed command or interaction.

## Carried from the daemon phases

- The Phase 0 gate line asking every GUI-parity capability for a target interaction was answered only by the deferral list in `BACKLOG.md`; filling the `GUI location` column is that half of the line.
- M3, drafts: `draft.set_recipients` is not built, so `DFT-11`'s recipient rewrite is client-side in the TUI.
- M3, attachments: there is no `message.attachments` method, so a client must know a part index before it can materialise a part; `w` on a server-only search hit forwards no attachments.
- M3, sending: `send.approved` and `send.outbox_retry` render the whole outcome at the end rather than per message, although the progress events already carry what a per-message view needs.
- M1, sync: `mp watch --mailbox` is INBOX-only, and the daemon has no periodic tick that keeps a store fresh with no client connected.
- M4, contacts: `contact.search`'s refused-rebuild guard is only logged daemon-side, so a client cannot say a rebuild was refused.
- M4, signatures: `signature.changed` has no `signature.removed` twin, so a deleted signature stays in every client until it re-bootstraps.
- The daemon reaches into `mp-tui` for `build_mailboxes` and `resolve_date`; both lift into `mp-core` before a third client depends on the arrow.

## Settled deferrals

Auto-mark-read, which #0110 retired, is not restored, and `BACKLOG.md` records it; the light theme (`OBS-07`) shipped in #0136.
The inline images and the rich HTML render #0109 and #0111 retired from the TUI come back in the GUI reader, which loads `message.html` with its `cid:` images inlined as `data:` URIs in a sandboxed frame (`RD-05`).

## M2 landed

M2, mutations with the undo hold, landed on the `gui-m2` branch on 2026-09-30, in the commits after `a127ce3a`, each tagged `(#0131)`.
It closes `MSG-01` to `MSG-09`, `SND-04`, `SYN-06` and the archive half of `LST-09`, whose `GUI location` now names the surface that shipped.

What shipped:

- Tauri commands `message_archive`, `message_delete`, `message_move`, `message_set_flag`, `message_set_read`, `draft_discard`, `send_hold_status`, `send_cancel_hold` and `sync_trigger`.
- Every message and draft command takes a batch, sends one daemon call per row in order with `settle: false`, fails a row alone on a `-32602` refusal, and stops the batch on an error about the whole batch.
- Fixture handlers for all of them, with a mutation journal, the drain 1.5 s after an account's last mutation, the `rollback` and `rollback:<n>` simulations, the `hold` simulation, and a seeded 60 s hold in the bootstrap.
- A frontend model of what the daemon has not confirmed: pending changes per row and per axis, the list generation guard with the pending overlay on every list answer, marks, holds, syncs, and activity notices capped at 20.
- The keys `a`, `d`, `u`, `*`, `M`, `v`, `Ctrl+a`, Escape for the marks, `ss`, `sS` and `X`, a palette entry for each action, and a DESKTOP section in the key help.
- The archive and delete confirmation, the move picker, row controls (mark box, unread and flag toggles, a pending mark), the "N marked" count and the reader toolbar.
- The activity area with live regions, and the send-hold countdown with Cancel.
- Mark read on an explicit open: Enter, a double-click, or Tab into the reader.
- 168 vitest tests (79 at M1) and 78 Rust tests (57 at M1), plus two ignored: the live-daemon test and the ts-rs export `pnpm gen:types` runs.

Decisions taken in M2:

- Archive asks for confirmation as delete does, mirroring the TUI.
- There is no undo for archive, delete or move, because the daemon has none; a rollback notice is the only undo surface.
- `u` cancels the newest held send while one is live, as the TUI's `u` does, and toggles read otherwise.
- `X` dismisses the newest activity notice; the TUI binds `X` only as the continuation of `cX`, so a bare `X` is free.
- Quick and full sync (`ss`, `sS`) and draft discard ship in M2, since `SYN-06` and `MSG-02` need them.
- The outbox view waits for M3, and `SYN-06` ships without a queue depth.

Review findings fixed before the close-out:

- Only a `-32602` refusal fails one row of a batch; any other error fails the whole batch.
- The fixture's `moved_to.selector` keeps the daemon's percent-encoded bracketed Message-ID.
- A pending change keeps one saved state per axis of its row, so a flag and a read in flight fail each on its own.
- Every restore re-reads the list and the counts, since the saved index is a guess once the list changed.
- The notices and the marked count render into live regions mounted empty, before their first text.
- The model keeps the newest 20 notices, and `X` and the palette dismiss the newest one or all of them.
- A `u` while every live hold is already being cancelled says nothing, as the TUI re-queues its cancel silently.
- The hold-cancel test skips a tick that lands after the cancel.

Known limits carried:

- App keys stop inside the cross-origin reader frame until a click returns focus to the app.
- List windowing is off, so every row of a mailbox is mounted, and `Ctrl+a` over a large one marks them all.
- A row moved into a mailbox the window shows appears there only after the drain's `state.invalidate`.

Open for M3:

- The outbox view (`SND-06`), and with it a queue depth for `SYN-06`.
- A `message.fetch` command for server-only search hits (`LST-09`'s `f`), which the palette badges "later" today.
- Search hits streamed from the server are not laid under the pending changes, so a hit for a row still leaving can show.
- A hold card's "Sent" and "Send cancelled" line is a `role="status"` that mounts with its text, which a screen reader may not announce.
- The fixture's `home` account fails its sync login by design, as its sync health says, so `ss` there always ends in a failure notice.

## M3 landed

M3, compose through the external editor, landed on the `gui-m3` branch on 2026-09-30, in the 21 commits from `b765c620` to `efe865b5`, each tagged `(#0131)`.
It closes `DFT-01`, `DFT-03` to `DFT-09`, `DFT-12`, `SND-01` to `SND-03`, `SND-06` to `SND-09`, `ATT-01`, `ATT-04` and `ATT-05`, and the M3 halves of `SND-04`, `MSG-07`, `SYN-06`, `MBX-07` and `LST-09`.
`DFT-02`, `DFT-10`, `DFT-11`, `ATT-02`, `ATT-03`, `ACC-09` and `ACC-11` ship in part, and [docs/parity-matrix.md](../parity-matrix.md) names what each lacks.

What shipped, per unit:

- U1, drafts and the editor (`b765c620`, `5eeb9826`, `a95ff466`): the Tauri commands `draft_create`, `draft_reply`, `draft_forward`, `draft_from_message`, `draft_path`, `draft_approve`, `draft_demote`, `draft_validate`, `draft_preview`, `draft_set_recipients`, `signature_list`, `editor_open`, `editor_setting_get` and `editor_setting_set`, and fixture drafts that are real files in a per-run directory, rescanned on every call.
- U2, compose (`cf3daa4e`, `35b4e025`, `71765368`): the `cn` wizard with To, Cc, Bcc, Subject and a signature select, reply and forward into the editor, a server-only hit replied to or forwarded through `draft_from_message`, `e` and `ce` on a draft, approve and demote as a pending `status` axis, the editing banner, draft rows with status, editing, sending and `invalid` badges, and the draft preview in the reader.
- U4, send (`898cc5d8`, `37b413a4`, `683a8cc4`): `send_draft`, which validates, approves a `draft` status and starts `send.draft` with `hold: true`, `send_approved`, the operation kinds `send` and `send_approved`, the TUI's confirmation texts, and the hold card's ends "Sent", "Send cancelled", "Failed" and "Partly delivered".
- U5, outbox (`9f4e3ff2`, `7523e2b7`, `0d21511a`, `2c450be4`): `outbox_list`, `outbox_retry` awaited as the operation kind `outbox_retry`, `outbox_discard`, the outbox view in the list pane with its state chips, Retry and Discard behind a confirmation that warns about a second delivery, the sidebar's outbox line as a button with a partly delivered count, and the queue depth in the status region.
- U3, attachments and fetch (`81785dfc`, `7a11e44d`, `efe865b5`): `attachment_open`, `attachment_save`, `html_open`, `hit_html_open`, `draft_attachments`, `draft_attach`, `draft_attachment_remove`, `draft_attachment_open` and `message_fetch`, the reader's attachment list with Open and Save, the Save and Attach file dialogs with a typed path, the draft preview's attachment list with Open and Remove, and the Fetch button on a server-only hit.
- The TUI's keys `cn`, `r`, `cr`, `ca`, `cf`, `e`, `ce`, `cA`, `cD`, `x`, `cX`, `to`, `ts`, `tb` and `ta`, each with a palette entry; the palette has no M3 badge left.
- The desktop's keys `go` for the outbox, `R` and `d` on its cursor row, and `F` for the fetch, since `f` is the find family's prefix in the desktop.
- The fixture simulations `editor_save` and `editor_invalid` (a save or a broken frontmatter in the file the last `editor_open` named), `send_fail`, `send_partial` and `send_pending_append` (the next send or retry fails, refuses a recipient, or leaves its Sent copy owed), and `send_hold:<secs>` (the fixture's `email.send_hold_secs`, `0` for no hold).
- 286 vitest tests (168 at M2) and 135 Rust tests (78 at M2), plus the same two ignored.

The unit docs are `clients/desktop/docs/rust-layer.md` ("Drafts and the editor", "Sends", "The outbox", "Attachments", "Fixture mode"), `clients/desktop/docs/shell.md` ("Compose", "Outbox") and `clients/desktop/docs/reader.md` ("Attachments", "The browser rendition", "Drafts and server-only hits").

Decisions taken in M3, each the breakdown's default:

- The editor is resolved from `MP_DESKTOP_EDITOR`, then the `editor` key of `desktop.json`, then `$VISUAL` and `$EDITOR` less terminal editors, then `code`, `zed`, `subl` and `cursor` in the Homebrew and system directories, then `open -t` (D1, D2).
- A terminal editor runs only through a terminal command template such as `wezterm start -- hx {path}`, and the app never waits for the editor to exit.
- Compose stays in the external editor until M5, and the wizard has no inline body, since `draft.create` takes none (D5).
- A draft's attachments are a client-side frontmatter rewrite, since the daemon serves neither `draft.attach` nor a removal (D3).
- The outbox view offers Retry and Discard behind a confirmation, although `SND-07` does not require them (D4).
- The outbox opens with `go`, the palette or the sidebar's outbox line (D6).
- `x` always asks first, and every send passes `hold: true`, so the daemon's `email.send_hold_secs` decides the window (D7).
- The dialog plugin is not installed, and typed path fields stand in for the native picker (D8).
- The fetch of a server-only hit ships in M3, on `F` (D9).
- `ts` on a draft says its files are already on disk and saves nothing.
- `d` on a draft file that does not parse names the file to delete by hand, since `draft.discard` takes no path, where the TUI deletes the file itself.

Review findings fixed before the close-out:

- The editor probe test fails if the probe goes directory by directory (`f0f2585e`).
- The wizard's focus-trap test waits for focus to settle inside the dialog after each Tab (`515cf825`).
- `d` on a draft that does not parse calls nothing, and a marked batch with one is refused whole (`d72ef02d`).
- While the outbox view shows, no key or palette row acts on the hidden mailbox selection, and Escape closes the view before it clears any marks (`864cb9e6`).
- The queue depth counts a retry of a row the listing already counts as open once, and `Ctrl+a` over the view marks nothing (`a1c3ef5e`).
- M2's open item on the hold card: its end line is a `role="status"` mounted empty with the card (`37b413a4`).

Known limits carried:

- Search hits streamed from the server are still not laid under the pending changes.
- App keys stop inside the cross-origin reader frame, and list windowing is off.
- An editor open on a draft can overwrite an attach, a removal or a `ce` rewrite with its own buffer, as in the TUI.
- `cX` sends one account's approved drafts; the CLI's `--all-accounts` loop has no GUI path.
- `send.approved` and `send.outbox_retry` still render their outcome at the end.

Open for M4 and for Sylvain:

- Install `tauri-plugin-dialog` to replace the path text fields in the Save and Attach file dialogs: the crate `tauri-plugin-dialog = "2"`, `.plugin(tauri_plugin_dialog::init())` in the builder, the npm package `@tauri-apps/plugin-dialog`, and the capability `dialog:allow-open` in `src-tauri/capabilities/default.json`.
- Check by eye with `MP_DESKTOP_FIXTURE=1 pnpm tauri dev`: the wizard, the editing banner, the draft preview, the hold card's outcomes, the outbox view and the attachment dialogs; nothing of M3 was run in a real window.
- Confirm the editor resolution order above: `MP_DESKTOP_EDITOR`, `desktop.json`, `$VISUAL` and `$EDITOR` with terminal editors skipped, the `code`, `zed`, `subl` and `cursor` probes, `open -t`.
- Confirm that the outbox's Retry and Discard sit behind a confirmation.
- Confirm the desktop keys `go`, `R` and `F`.
- Confirm that `ts` on a draft is declined.
- Confirm that `d` on a draft file that does not parse shows a notice instead of deleting the file, as the TUI does.
- `REQUIRED_CAPABILITIES` in `clients/desktop/src-tauri/src/connector.rs` lacked the M3 methods, so a daemon without them connected and failed at the first call: fixed in `30a9f51c`.
- `draft.reply` and `draft.forward` take no signature arguments, so a reply or a forward gets no signature choice: they always took `signature` and `no_signature`, and the forward wizard passes its choice now ("Daemon gaps closed").
- `draft.create` takes no body, so the wizard cannot write one: it takes `body` and `headers` now, and the wizard has the TUI's inline body ("Daemon gaps closed").
- `DraftEntry` has no `bcc`, so the recipients dialog reads the draft through `draft_preview`: it has one now, and `ce` fills its dialog from the listed row ("Daemon gaps closed").
- `DraftCreated` has no `subject`: it has one now ("Daemon gaps closed").
- `mp_client` drops a refusal's `data`, so the layer rebuilds the `draft.invalid` payload from the listing's skipped file: it keeps it now, and the layer decodes it ("Daemon gaps closed").
- The daemon's `SendOutcome.message_id` is empty: it is the built message's now ("Daemon gaps closed").
- The daemon serves no `signature.list`, so `signature_list` reads the signatures directory itself.
- The daemon serves no `draft.attach` and no attachment removal, so the desktop rewrites the frontmatter itself.
- `message.fetch` is polled through `operation.status` every 100 ms rather than awaited as a pending operation.
- The draft preview's Approve and Back to draft buttons acted on the marks the outbox view hides while it is open (the fix1 review), against `clients/desktop/docs/shell.md`, "What the view hides": fixed in `683bcfac`, they act on the draft shown.

## M4 landed

M4, calendar, contacts, signatures and config, landed on the `gui-m4` branch on 2026-09-30, in the 47 commits from `f888afce` to `41e3b73f`, each tagged `(#0131)`.
It closes `CAL-01`, `CAL-02`, `CAL-03`, `CAL-05`, `CAL-06`, `SND-05`, `CON-01`, `CON-03`, `CON-05` to `CON-08`, `ACC-01`, `ACC-02`, `ACC-05`, `ACC-06`, `ACC-10`, `INT-01` to `INT-03`, `OBS-01` and `MBX-05`, the signature half of `MSG-07`, and the OAuth half of `INT-04`.
[docs/parity-matrix.md](../parity-matrix.md) now also records `RD-07`, `OBS-02` and `OBS-03` as shipped in M1 and `INT-06` in M3; `RD-08`, the Markdown rendition path of a search hit, is not built.

What shipped, per unit:

- U1, views, copies and progress (`f888afce` to `3528ecf5`): `state.view` with Mail, Contacts, Calendar and Settings, `switch_view`, the per-view key tables read before any prefix arms (`VIEW_KEYS`, `VIEW_AGNOSTIC_COMBOS`), the sidebar's view entries and palette rows, `src/lib/clipboard.ts`, through which every copy goes, `operation.progress` routed to this window's awaited operations only and kept in `state.progress`, and `FIXTURE_ONLY_METHODS` for the fixture's pseudo-methods.
- U2, the agenda (`c322720a` to `1748d111`): `calendar_events` and `invite_source_open`, the Calendar view with its agenda and the shared `EventCard`, the past toggle by the TUI's rule, the refresh, the source `invite.ics` in the editor, and the fixture's `calendar.json` with the simulations `invite_update` and `invite_cancel`.
- U3, RSVP and invitations (`d0969eb7` to `eb5b3c72`): `invite_get`, `calendar_rsvp`, `invite_refusal` and `send_invite`, the reader's invitation card, the RSVP choice from `tv`, `V` and the card, the New invitation form, disabled on a Graph account, the operation kinds `rsvp` and `send_invite`, `error::refusal_sentence` and `daemon_sentence`, and the simulation `rsvp_fail`.
- U4, contacts (`4c94b299` to `3f3fb3e6`): `contact_search`, `contact_rebuild` awaited as `contact_rebuild` with the TUI's four notices, `contact_vcard_draft`, which builds the vCard in the Tauri layer, the Contacts view with its debounced search, the fixture's `contacts.json`, and the simulation `rebuild_refused`.
- U5, signatures (`1bf92ed9` to `9c8e2dec`): `signature_read`, `signature_create`, `signature_rename`, `signature_delete` and `signature_set_default` over `mp_core::signatures`, the Signatures dialog with its delete confirmation, the listing the new-draft wizard shares, the fixture's `signature.*` pseudo-methods, and the simulation `signature_changed`.
- U6, activity log, daemon files and copies (`057faeda` to `98acad6f`, with a U5 test fix in `ae5bff47`): `config_open` and `log_open`, the activity log of the newest 100 lines and its dialog, `!` over the activity area's notices, the reader's Copy menu with its palette rows, and the fixture's `config.json`, `config.toml` and dated log file.
- U7a, Settings (`b66d1ef2` to `cd2968f1`): `config_get` decoded leniently into `EffectiveConfig`, `config_reload`, `config_set_password` with a redacting `Debug`, the Settings view with the M3 editor setting, the config.toml banner, the password dialog, and the simulation `config_invalid`.
- U7b, accounts and sign-in (`d830f4d9` to `b87a9e56`): `config_init`, `config_add_account`, `config_oauth2_login` awaited as `oauth2_login` and `config_oauth2_cancel`, the account wizard with the CLI's four presets, the device-code dialog, the first-run setup screen, and the simulations `config_absent`, `oauth_approve` and `oauth_deny`.
- The TUI's keys `Space m`, `Space c`, `Space a`, `cs`, `sl`, `sc`, `sf`, `!` and `tv`, the CALENDAR keys `j`, `k`, `gg`, `G`, Enter, `e`, `t`, `r` and `V`, and the CONTACTS keys `j`, `k`, `gg`, `G`, `/`, Enter, `n`, `v`, `c` and `r`, each with a palette entry; the palette has no M4 badge left.
- The desktop's palette rows with no key: "Open settings", "Add account", "New invitation", "Copy sender address" and "Copy subject", with "Copy link (mp://)" on `y`.
- The daemon methods M4 calls joined `REQUIRED_CAPABILITIES`: `calendar.events`, `message.ics`, `message.invite`, `calendar.rsvp`, `send.invite`, `contact.search`, `contact.rebuild`, `config.get`, `diagnostic.log_path`, `config.reload`, `config.set_password`, `config.init`, `config.add_account` and `config.oauth2_login`.
- 483 vitest tests in 34 files (286 at M3) and 205 Rust tests (135 at M3), plus the same two ignored.

The unit docs are `clients/desktop/docs/rust-layer.md` ("The calendar", "The contacts", "Signatures", "config.toml and the daemon log", "Configuration and secrets", "Fixture mode"), `clients/desktop/docs/shell.md` ("Views", "Contacts", "Calendar", "Signatures", "Activity log", "Settings", "Account wizard", "First run") and `clients/desktop/docs/reader.md` ("The toolbar", "Invitations").

Decisions taken in M4, each the breakdown's default:

- No plugin was installed: every copy calls `navigator.clipboard.writeText` from its key or click handler, and M3's typed path fields stay until the dialog plugin is approved (D1).
- The vCard is built in the Tauri layer with `mp_core::contacts::contact_to_vcard` and attached to a `draft.create` draft, since the daemon serves no `contact.vcard` (D2).
- The signature commands run over `mp_core::signatures` in the Tauri layer, since the daemon serves no `signature.*` method (D3).
- Settings reads `config.get` and writes no key: a setting changes in config.toml through the editor, then Reload (D4).
- New invitation is a minimal form with no preview and no UID of its own, disabled on a Graph account (D5).
- The past and upcoming filter is client-side, the TUI's rule (D6).
- `Space m`, `Space c` and `Space a`, the sidebar and the palette switch the views, and Settings is a fourth view (D7).
- The wizard offers the CLI's four presets, writes through `config.add_account`, or `config.init` on first run, then stores a password or signs in, with no connection test and no server mailbox pick (D8).
- No secret reaches the clipboard; the device-code dialog's Copy copies the user code (D9).
- Of the stale status lines outside M4, the matrix corrects `RD-07`, `RD-08`, `OBS-02`, `OBS-03` and `INT-06` only (D10).
- Settings has no key (D11).
- `!` hides or shows the activity area's notices and never a live hold card (D12).
- `contact.search` asks for 1000 rows, the empty query included (D13).
- The copies have no key of their own, and `y` stays the selector (D14).

Decisions taken where the breakdown was silent:

- A settled RSVP says "Replied <response> to <summary>", with the summary captured when the reply was asked, and adds "; queued in the outbox" when no recipient took it yet.
- The TUI's em-dash in the RSVP refusals is a semicolon on the desktop.
- Enter in the RSVP choice sends whichever control holds the focus.
- `contact_vcard_draft` takes a draft name the frontend computes, and the vCard draft carries no signature, the TUI's rule.
- An empty query's score (`u32::MAX`) is not shown, and the contact list is read again on every switch to the view.
- `c` on a contact says "Copied <address>" where the TUI says "<address> copied to clipboard".
- The Contacts header's buttons render only while a contact is under the cursor, since disabled buttons broke the focus following.
- A second `r` during a rebuild says it is already running, and a dropped rebuild says "Contacts refresh failed: <why>".
- The signature delete confirmation is the shared confirmation, nested in the Signatures dialog.
- Signature refusals show in the dialog's alert line and successes on the notice line, and a refused change reads the listing again.
- `cs` works over the outbox view on the outbox's account, and "No signature named" is `not_found`.
- The Signatures and RSVP dialogs read their keys in the capture phase, since Base UI stops the arrow keys first.
- A hold's end is logged through the hold's own transitions, and a sync line is the TUI's `sync_status_line` at the daemon's severity.
- A `not_found` from `sc` or `sf` is logged as a warning.
- While `!` hides the notices, every hold card stays and `X` dismisses nothing.
- "Copy link (mp://)" copies the reader's selector and lists `y`, the copies hide outside Mail and over the outbox view, and a palette copy needs the reader to show the selected message.
- The keymap ignores keys inside an open menu (`role="menu"`).
- The fixture's password key is the daemon's, `<kind>-password-<account>`.
- A reload's notice reuses the `config.changed` wording and is not logged twice.
- The password field is emptied on every submit and focused again after a refusal, and the configuration is read on every open of Settings.
- The config.toml banner is a `role="alert"` mounted only while a problem exists.
- Settings carries M3's editor command field, whose Save with an empty field clears it.
- A stored token says "OAuth2 token acquired and cached for account '<name>'", `oauth2_stored_line` without its check mark.
- The wizard's password step stores the SMTP password only, which IMAP falls back to.
- An account name matches `^[A-Za-z0-9][A-Za-z0-9._-]*$`, and an empty IMAP host or username falls back to the SMTP one, as in the CLI.
- The Graph preset's mailboxes are Inbox, Archive and Sent Items.
- The first-run wizard sits inline on the setup screen without Cancel, and a window that started with no account selects the first account added and its inbox.
- Escape and Cancel cancel a running sign-in through the dedicated `config_oauth2_cancel`, and Settings' Sign in reopens a running sign-in.

Review findings fixed before the close-out:

- A failed `r` in the Calendar view with rows shown says "Calendar refresh failed: <why>" (`46c2c67c`).
- A daemon refusal of an RSVP start reads as the daemon's sentence (`dbe2e2de`).
- An in-view account switch reads the kept query's contacts again, and an answer asked at an older generation is dropped (`33cd299e`).
- `markStale` always moves `gen`, so an event during a fetch makes the loader fetch again, and shell.md's Model section says so (`8323e6f5`).
- The RSVP choice reads the arrow keys in the capture phase (`3f21393c`).
- The queue depth counts RSVPs and invitations under "sending" (`1d2a5505`).
- A failed sync this window started is logged once (`07bfc027`).
- The config.toml banner goes with a daemon restart to another instance and comes from `config.get`'s `invalid` state when the daemon started on a bad file (`95b77372`).
- A stored password closes only its own dialog, and a wizard dismissed during its write hands over to no dialog (`41e3b73f`).

Known limits carried:

- Search hits streamed from the server are still not laid under the pending changes.
- App keys stop inside the cross-origin reader frame, and list windowing is off.
- An editor open on a draft can overwrite an attach, a removal or a `ce` rewrite with its own buffer, as in the TUI.
- `cX` sends one account's approved drafts, and `send.approved` and `send.outbox_retry` still render their outcome at the end.
- The activity log holds only what this window heard since it started.
- The contact list stops at 1000 rows, and the fixture's contact search matches substrings where the daemon's is fuzzy.

Open for M5 and for Sylvain:

- Check by eye with `MP_DESKTOP_FIXTURE=1 pnpm tauri dev`: nothing of M2 to M4 has run in a real window.
  The M4 checks are `writeText` in WKWebView from a key press and from a Base UI menu click, the focus coming back from the editor after `sc`, `sf`, `e` on an agenda row and a signature's Edit, `editor_open` on a `.ics`, a `.toml` and a `.log`, nested dialogs and Escape in WebKit, the view switch in the narrow layout, the setup screen with a daemon started on an empty config directory, and the device-code flow against a live provider.
- Install `tauri-plugin-clipboard-manager` only if the window check shows `writeText` refused; the dialog-plugin install from "M3 landed" still waits for approval.
- Confirm the M4 decisions above, and the M3 ones still open under "M3 landed".
- `contact_vcard_draft` creates the draft before it writes and attaches the `.vcf`, so a failure after the create leaves a "Contact: <name>" draft in Drafts without the vCard and unopened, while the notice says only "vCard draft failed: <why>"; writing the `.vcf` before `draft.create`, or naming the leftover draft in the notice, would fix it.
- The queue depth can count a send, an RSVP or an invitation twice, under "sending" and in the outbox, when the outbox listing is read again while the operation runs, as M3's sends already could.
- `calendarLoaded`, `inviteLoaded` and the generic `loaded()` still take an answer asked at an older generation; the next fetch heals it, and only contacts needed the guard.
- A failed sync is still logged twice when the operation's end arrives before `sync_trigger` answers.
- `compose.ts` (lines 300 and 316) and `attachments.ts` (lines 226 and 247), from M2 and M3, still close whatever overlay is open when their call answers; the matching close `close_password` does would fix them.
- The Rust test's `ACCOUNT_BLOCK_KEYS` is a hand-written copy of the keys the daemon's `account_block` reads at this commit, and does not notice a change there.
- The fixture's bootstrap sends an empty `operations`, so the device code after a re-bootstrap is covered by TypeScript tests only; an account the fixture adds has no Drafts mailbox; the daemon's `state.invalidate` after `config.add_account` is not modelled.
- Timed notices do not expire while `!` hides them, so up to 20 reappear, and clipboard refusals and older failure notices are logged at `info`.
- A failed `operation.cancel` (a timeout) leaves the sign-in dialog's cancelling state set until the operation ends.
- `ACC-11`'s signature choice for a forward shipped with the daemon gaps ("Daemon gaps closed"), and a reply carries the default as in the TUI; `ACC-03`'s question whether a password or a token is stored has no answer in `config.get`.
- The daemon door of the five signature commands has no Rust test, which needs `mp-core`'s test-support override of the config directory.
- `EmptyView.tsx` is used by no view any more and is kept.
- The daemon follow-ups M4 worked around are in `BACKLOG.md`: a `signature.*` family with `signature.removed`, `contact.vcard`, an event when a contact index changes (`CON-08`), and a signal for `contact.search`'s refused implicit build.
- The Tauri layer calls `operation.cancel` through two dedicated commands, `search_server_cancel` and `config_oauth2_cancel`; a generic cancel command would serve a rebuild, an RSVP or an invitation too, and `BACKLOG.md` carries it as optional.

## Daemon gaps closed

The daemon and Rust-layer gaps M3 and M4 worked around client-side, closed on the `p-2026-10-01` branch on 2026-10-01, one commit each, tagged `(#0131)`, with the TUI unaffected.

- Signatures for a reply and a forward: `draft.reply` and `draft.forward` always took `signature` and `no_signature`, which `tests/daemon_gui_gaps.rs` now pins; `draft_reply` and `draft_forward` pass them, and the forward wizard has the new-draft wizard's Signature select.
- A body on `draft.create`: it takes `body` and `headers`, so the new-draft wizard's draft is written whole by the daemon, the client-side recipients rewrite is gone, and the wizard has the TUI's inline Body, which skips the editor when filled.
- `bcc` on `DraftEntry`: a `draft.list` row carries the file's `bcc:`, and `ce` fills the recipients dialog from the listed row instead of a `draft_preview` read.
- `subject` on `DraftCreated`: every writer answers the subject the file was written with; the desktop had no workaround for it, and decodes it with the rest.
- The refusal `data` `mp_client` dropped: a blocking session call that the daemon refused answers an `anyhow::Error` wrapping `mp_client::session::Refused`, whose text is unchanged and whose `RpcError` keeps `data`; `draft_approve`, `draft_demote` and `send_draft` decode the `draft.invalid` payload from it instead of reading `draft.list` for the skipped file, and the fixture's refusals carry the same type.
- The empty `SendOutcome.message_id`: `send.draft` and `send.approved` answer the `Message-ID` the build minted, which `send.invite` already did; the desktop's fixture always filled it, so no shim went.

## Exit gate

- Every GUI-parity row is implemented and validated, or carries a settled deferral recorded in `BACKLOG.md`.
- Every current TUI user capability has a GUI path.
- CLI automation, diagnostics, daemon-administration and migration-only classifications carry an explicit rationale.
- TUI parity remains green.
