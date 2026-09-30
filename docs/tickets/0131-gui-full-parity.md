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

The first release is dark-only (`OBS-07`), and auto-mark-read, which #0110 retired, is not restored; `BACKLOG.md` records both.
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
- `REQUIRED_CAPABILITIES` in `clients/desktop/src-tauri/src/connector.rs` lacks the M3 methods, so a daemon without them connects and fails at the first call.
- `draft.reply` and `draft.forward` take no signature arguments, so a reply or a forward gets no signature choice.
- `draft.create` takes no body, so the wizard cannot write one.
- `DraftEntry` has no `bcc`, so the recipients dialog reads the draft through `draft_preview`.
- `DraftCreated` has no `subject`.
- `mp_client` drops a refusal's `data`, so the layer rebuilds the `draft.invalid` payload from the listing's skipped file.
- The daemon's `SendOutcome.message_id` is empty.
- The daemon serves no `signature.list`, so `signature_list` reads the signatures directory itself.
- The daemon serves no `draft.attach` and no attachment removal, so the desktop rewrites the frontmatter itself.
- `message.fetch` is polled through `operation.status` every 100 ms rather than awaited as a pending operation.
- The draft preview's Approve and Back to draft buttons act on the marks the outbox view hides while it is open (the fix1 review), and `clients/desktop/docs/shell.md`, "What the view hides", says nothing hidden acts on them.

## Exit gate

- Every GUI-parity row is implemented and validated, or carries a settled deferral recorded in `BACKLOG.md`.
- Every current TUI user capability has a GUI path.
- CLI automation, diagnostics, daemon-administration and migration-only classifications carry an explicit rationale.
- TUI parity remains green.
