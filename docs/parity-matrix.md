# Feature-parity matrix

Every capability the current CLI and TUI deliver, one entry per stable identifier, carried from the verified capability inventory of phase 0 of the daemon migration (ticket #0118).
The identifiers are stable: they carry into the phase checklists, the protocol fixtures, and the test names, and a retired capability keeps its identifier reserved so an older document cannot silently rebind it.

Built at `f8af44b` (the `pre-daemon` tag), from the artifacts in `docs/baselines/pre-daemon/` and from the source tree itself.
Every source anchor below was resolved against the tree; the ones that had moved are listed under [Anchor corrections](#anchor-corrections).

## How to read an entry

The heading carries the identifier and the capability.
Each entry then holds the remaining columns of the matrix:

- Classification: one value from the vocabulary below.
- Source anchor: the command, key, file, and symbol that deliver the capability today.
- Daemon surface: the methods, events, and resources the capability needs after the cutover.
- GUI location: the intended GUI surface and interaction.
- Validation: the coverage that exists today.
- Status: implementation and validation state.

The matrix is a list rather than a table because eight columns over 131 rows is unreadable in an editor; the field names are the columns.

### Classification vocabulary

- GUI parity: a user-facing capability the GUI must deliver, in milestones M1 to M4 of the [native GUI plan](plans/native-gui.md), and M6 for desktop notifications.
- CLI automation: a machine-facing surface whose consumers are scripts, agents, and other tools.
- Diagnostics and maintenance: an operator surface for inspecting or repairing local state.
- Daemon administration: lifecycle, locking, watching, and queue operation of the daemon itself.
- Migration-only: a one-way conversion surface that disappears once every user has run it.

### Daemon surface

The method families are fixed by the plan: `state.*`, `account.*`, `mailbox.*`, `message.*`, `draft.*`, `send.*`, `sync.*`, `contact.*`, `calendar.*`, `signature.*`, `config.*`, `operation.*`, `diagnostic.*`, `daemon.*`.
Only nine method names are fixed by the plan itself: `state.bootstrap`, `state.event`, `state.resync_required`, `account.list`, `message.list`, `message.jump_to_date`, `message.filter`, `message.select_all`, and `draft.discard`.
Every other name in this document is a proposal at family level, pinned by the P2-U2 protocol fixtures and free to change until then; the commitment is the family.
A "client-side" entry needs no method at all and stays in the client process.

### GUI location

Each GUI-parity entry names `clients/desktop` and the milestone of the [native GUI plan](plans/native-gui.md) planned to deliver it, with its ticket: M1 is #0129, which shipped the shell together with the read slices first planned for #0131, M2 to M4 are #0131, and M6 is #0132.
An entry with another classification reads `not required`, since the plan puts no GUI-parity obligation on it.
The ticket that ships an entry replaces the milestone with the surface and interaction it built.

### Validation

Each entry names the coverage that exists today.
On top of that, and not repeated per entry: every daemon-served capability gains a protocol contract test in Phase 2 or later, and every GUI-parity capability gains a GUI and an end-to-end check in the milestone that delivers it.

### Status vocabulary

- `not started`: inventoried here, no daemon or GUI work done.
- `routed (<unit>)`: the CLI surface answers from the daemon, byte-identically to the pre-daemon binary, with the unit that moved it named.
- `retired`: the capability was removed from the product; the identifier stays reserved.
- `deferred`: parity is agreed but scheduled out of the first GUI release, with the deferral recorded in the backlog.
- `GUI shipped (<milestone>, <ticket>)`: the desktop client delivers the capability, validated by the `clients/desktop` tests the entry names, which run against the Tauri layer's fixture daemon unless the entry says otherwise.

## Accounts, configuration, secrets, and signatures

### ACC-01 Interactive account setup wizard

- Classification: GUI parity
- Source anchor: `mp config init`, `src/main.rs`, `src/config_cmd/init.rs`
- Daemon surface: `config.get` before the first prompt, then `config.reload` once the wizard has written; `config.init`, `config.set_password`, `operation.*` for the multi-step pass, `state.event` once the account exists
- GUI location: clients/desktop: the Settings view's and the palette's "Add account" open the account wizard with the CLI's four presets (IMAP and SMTP, Proton Bridge, Microsoft 365 OAuth2, Microsoft 365 Graph), then identity, servers, mailboxes and a review that calls `config_add_account`, or `config_init` from the first-run setup screen a daemon with no config.toml shows; a password preset then asks for the SMTP password, and a Microsoft 365 preset starts the device-code sign-in (M4, #0131; shell.md, "Account wizard" and "First run")
- Validation: `tests/daemon_admin_slice.rs` (`mp_config_init_prompts_in_the_client`); `clients/desktop/src/components/settings/wizard.test.tsx` (`refuses to move on without a name, with a taken one, and without the SMTP host`, `adds a password account, then stores its SMTP password with no password in the store`, `shows the setup screen on a daemon with no config.toml, and its wizard writes the first one`), `clients/desktop/src/app/wizard.test.ts` (`every preset's draft has only account_block's keys and no password`, `follows the CLI's presets and fallbacks`), `clients/desktop/src-tauri/src/configuration.rs` (`every_presets_draft_serialises_to_account_block_keys_and_nothing_else`, `config_init_after_config_absent_writes_the_first_configuration`)
- Status: routed (P4-U14) at the edges; the wizard itself is client-side, recorded (P4-U15); GUI shipped (M4, #0131) without the connection test and the server mailbox pick, which both need an account the daemon already serves
- Note: the wizard writes `config.toml` and one secret in a single pass, so the GUI drives it through `config.*` rather than spawning the CLI.
  P4-U14 routed the branch a parity test can reach - the overwrite question, which the client asks after `config.get` has told it whether a configuration exists and where - and left the wizard's own writes in the client, which reload the daemon when they finish; the pass past the first prompt dials a mail server and is pinned by nothing.
  P4-U15 kept it there and recorded why: the prompting, the connection test the answers steer, and the write are one interactive transaction, and splitting it needs a wizard protocol no unit of Phase 4 contracted. It is five rows of `CLI_ENGINE_RESIDUE` in `tests/architecture_boundaries.rs` (`set_secret`, `imap_client::` twice, `GraphClient::`, `device_code_flow`, `SmtpTransport::`).

### ACC-02 Add a further account to an existing configuration

- Classification: GUI parity
- Source anchor: `mp config add-account`, `src/config_cmd/init.rs`
- Daemon surface: `config.get` before the first prompt, then `config.reload`; `config.add_account`, `state.event`
- GUI location: clients/desktop: the account wizard's review calls `config_add_account` while config.toml exists, a refusal such as a taken name stays in the wizard's alert, and the daemon's `config.changed` brings the account and its mailboxes into the sidebar (M4, #0131; shell.md, "Account wizard")
- Validation: `tests/daemon_admin_slice.rs` (`mp_config_add_account_refuses_without_a_configuration`); `clients/desktop/src/components/settings/wizard.test.tsx` (`adds a password account, then stores its SMTP password with no password in the store`, `shows the daemon's refusal and stays open`), `clients/desktop/src/app/settings.test.ts` (`config.changed reads the updated account's mailboxes and list again, and leaves the others`), `clients/desktop/src-tauri/src/configuration.rs` (`config_add_account_appends_the_block_and_serves_a_ready_account`, `a_draft_with_a_password_or_any_unknown_key_is_refused`)
- Status: routed (P4-U14) at the edges; the wizard itself is client-side, recorded (P4-U15); GUI shipped (M4, #0131)
- Note: the refusal when there is no configuration to add to is the daemon's answer, rendered here; the wizard past it is `ACC-01`'s note.

### ACC-03 Show the effective configuration

- Classification: diagnostics and maintenance
- Source anchor: `mp config show`, `src/config_cmd/show.rs`
- Daemon surface: `config.get`
- GUI location: not required (diagnostics and maintenance)
- Validation: `tests/daemon_admin_slice.rs` (`mp_config_show_matches_the_oracle`)
- Status: routed (P4-U14) for the configuration; three probes stay client-side, recorded (P4-U15)
- Note: the output is redacted, and after the cutover the daemon is the only reader of the underlying `config.toml`.
  Three things on the screen are still read locally, and stay so by `config.get`'s own contract: the `password = **** / (not set)` column (`get_secret`), the `token = valid|expired|invalid|not cached` line (`oauth2::load_token_cache`), and the signature listing. `tests/daemon_config.rs` contracts `config.get` *not* to probe a secret, because answering "is a password stored" for every account on every read is what turns a redacted read into a secret read; a tenth `config.*` method is pinned shut by `tests/daemon_admin_slice.rs`. Three rows of `CLI_ENGINE_RESIDUE`.
  One divergence from the `pre-daemon` binary is deliberate and older than this slice: `config.get` reports the effective configuration, which `docs/daemon-protocol.md` defines as the document *after serde defaults*, so `mp config show` prints `smtp.port = 465` and `imap.port = 993` for a configuration that omits them where the oracle printed `0`.
  The `ACC-03` parity row names every port in its fixture, which takes the difference out of the comparison and leaves the row measuring what it is about.

### ACC-04 Print the configuration file path

- Classification: diagnostics and maintenance
- Source anchor: `mp config path`, `src/config_cmd/mod.rs`
- Daemon surface: client-side; the path is computed, so the command must keep running with no daemon
- GUI location: not required (diagnostics and maintenance)
- Validation: `tests/daemon_admin_slice.rs` (`mp_config_path_never_contacts_a_daemon`)
- Status: client-side, confirmed (P4-U14)
- Note: on `needs_daemon`'s no-daemon list for good, and from P4-U14 the `UNMIGRATED` control row of `tests/daemon_parity_harness.rs`: it is the only *whole command* a daemon-era binary answers in process. The startup preamble every command runs, and the server leg of `mp search` (`LST-06`), are the other in-process code paths; both are rows of `CLI_ENGINE_RESIDUE` in `tests/architecture_boundaries.rs`.

### ACC-05 Store an SMTP or IMAP password in the active secrets backend

- Classification: GUI parity
- Source anchor: `mp config set-password <smtp|imap> [--account]`, `src/main.rs`, `src/config_cmd/password.rs`
- Daemon surface: `config.set_password`
- GUI location: clients/desktop: a password account's card in the Settings view has "Set SMTP password" and "Set IMAP password", which open a masked dialog whose Enter calls `config_set_password`; the value lives in the dialog's own state only and is emptied on every submit and close (M4, #0131; shell.md, "The password dialog")
- Validation: `tests/secrets_integration.rs` for the backend, `tests/daemon_admin_slice.rs` (`mp_config_set_password_reads_the_password_in_the_client`); `clients/desktop/src/components/settings/settings.test.tsx` (`takes a masked, unremembered value in a labelled field, focused on open`, `stores the password, says so without the value, and keeps it nowhere in the model`, `clears the value when it closes, and shows a refusal with the field empty for a retry`), `clients/desktop/src-tauri/src/configuration.rs` (`a_stored_password_is_in_no_journal_no_debug_and_no_tracing_line`, `a_refused_password_names_the_account_and_never_the_value`)
- Status: routed (P4-U14); GUI shipped (M4, #0131); the card does not say whether a password is stored, since `config.get` never probes a secret (`ACC-03`)
- Note: secret values travel only on the local socket and never appear in logs, diagnostics, or protocol errors.

### ACC-06 OAuth2 device-code login

- Classification: GUI parity
- Source anchor: `mp config oauth2-login [--account]`, `src/config_cmd/oauth2.rs`, `src/oauth2.rs`
- Daemon surface: `config.oauth2_login` as an `operation.*` whose one `operation.progress` event, phase `device_code`, carries the verification URL and the user code; `operation.cancel` settles it `cancelled` and leaves the provider's poll running
- GUI location: clients/desktop: "Sign in" on an OAuth2 or Graph account's card in the Settings view, and the account wizard's two Microsoft 365 presets, call `config_oauth2_login`, awaited as `oauth2_login`; the device-code dialog shows the code from that operation's progress with "Copy code" and "Open verification page", says "OAuth2 token acquired and cached for account '<name>'" once stored, and its "Cancel sign-in" and Escape call `config_oauth2_cancel` (M4, #0131; shell.md, "The device-code dialog")
- Validation: `tests/daemon_admin_slice.rs` pins the three refusals and the rendering; `clients/desktop/src/components/settings/wizard.test.tsx` (`a Microsoft 365 account signs in next: the code from the progress, Copy, the verification page, stored`, `shows the code a re-bootstrap's operation status carries when the progress event was missed`, `Cancel cancels the operation and says the sign-in may still complete`), `clients/desktop/src/app/signin.test.ts` (`reads the code from its own operation's progress and settles stored`), `clients/desktop/src-tauri/src/session.rs` (`a_device_code_reaches_only_its_awaited_sign_in_which_settles_as_oauth2_login`, `a_denied_sign_in_fails_with_the_providers_sentence`, `a_cancelled_sign_in_stays_awaited_until_its_cancelled_finish`), `clients/desktop/src-tauri/src/configuration.rs` (`a_sign_in_is_refused_with_the_daemons_sentences`); the flow against a live provider is manual
- Status: routed (P4-U14); GUI shipped (M4, #0131) against the fixture's simulated provider
- Note: the client renders the code and opens the browser as a client-side integration (`INT-04`).
  The progress payload is `{phase: "device_code", done: 0, total: null, message: "<verification_uri> <user_code>"}`, and `mp_client::format::{oauth2_start_line, oauth2_device_code_lines, oauth2_stored_line}` are the three lines a GUI reproduces.
  The IMAP, SMTP and Graph connection tests the command ran after acquiring a token are not on the routed path: they need the token the daemon now holds, and they belong to the account slice.

### ACC-07 Reset secrets, wiping the encrypted file and the OAuth token caches

- Classification: diagnostics and maintenance
- Source anchor: `mp config reset-secrets`, `src/config_cmd/reset.rs`
- Daemon surface: `config.reset_secrets`, then one `config.set_password` per re-entered credential
- GUI location: not required (diagnostics and maintenance)
- Validation: `tests/daemon_admin_slice.rs` (declined and confirmed)
- Status: routed (P4-U14)
- Note: the recovery path after a restore onto a new machine, where the machine-uid derived key no longer decrypts the file.
  The confirmation and every password prompt stay in the client; the answer names the secrets file first and then the token caches, in path order, where the pre-daemon binary walked `read_dir` unsorted.

### ACC-08 Secrets backend, keyring plus a ChaCha20-Poly1305 file keyed through HKDF from the machine uid

- Classification: daemon administration
- Source anchor: `src/secrets.rs`, `tests/secrets_integration.rs`
- Daemon surface: daemon-internal; only the daemon opens the backend after the cutover
- GUI location: not required (daemon administration)
- Validation: `tests/secrets_integration.rs`
- Status: not started

### ACC-09 Account and signature selection on every command

- Classification: GUI parity
- Source anchor: the global arguments `-A/--account`, `-s/--signature`, `--no-signature` in `src/main.rs` (`no_signature`, `src/main.rs:41`), `src/signatures.rs`
- Daemon surface: an account parameter on every domain method, `signature.list`, `state.bootstrap`
- GUI location: clients/desktop: the sidebar lists every account, and `ga` or a mailbox chosen in it selects the active account (M1, #0129); a new draft's signature is chosen in the `cn` wizard's Signature select, filled from `signature_list` with the account's default preselected and "none" last (M3, #0131)
- Validation: `tests/cli_help_snapshot.rs` pins the global arguments; `clients/desktop/src/app/reducer.test.ts` (`keeps a choice made between a failed list_accounts and its retry`, `stays with the user's account across a later re-bootstrap`), `clients/desktop/src/components/compose/compose.test.tsx` (`has the TUI's fields, the default signature preselected, and a none option`, `none carries no signature`)
- Status: GUI shipped (M1, #0129) for the account selector and (M3, #0131) for a new draft's signature; a reply or a forward has no signature choice, since `draft.reply` and `draft.forward` take no signature argument
- Note: the GUI equivalent is the active-account selector plus a per-composition signature choice.

### ACC-10 Signature file management

- Classification: GUI parity
- Source anchor: the TUI `cs` overlay (`clients/tui/src/app/keymap.rs:603`), `crates/mp-core/src/signatures.rs`, the per-account default recorded in `state.json`
- Daemon surface: `signature.list`, `signature.read`, `signature.write`, `signature.create`, `signature.rename`, `signature.delete`, `signature.set_default`
- GUI location: clients/desktop: `cs` in Mail and the palette's "Manage signatures" open the Signatures dialog, which lists every signature with the account's default, previews the cursor one, and creates (`n`), renames (`r`), edits in the external editor (`e`), deletes behind a confirmation (`d`) and sets or clears the default (Enter), through the Tauri layer over `mp_core::signatures`, since the daemon serves only `signature.list` of the family (M4, #0131; shell.md, "Signatures", rust-layer.md, "Signatures")
- Validation: `clients/desktop/src/components/signatures/signatures.test.tsx` (`cs lists every signature with the account's default marked and previews the selected one`, `Enter makes the selected signature the default, and clears it when it already is`, `n asks for a name, Enter creates the signature and opens it in the editor`, `r renames from a field seeded with the name, and the default follows`, `d asks first with the file's path; n keeps it, y deletes it`, `signature.changed reads the listing and the preview again while it is open`), `clients/desktop/src/app/signatures.test.ts` (`signature.changed, the dialog's own change and a bootstrap make every listing stale`), `clients/desktop/src-tauri/src/signatures.rs` (`create_writes_an_empty_file_and_refuses_a_name_taken`, `rename_carries_the_default_and_moves_the_file`, `deleting_the_default_clears_it_and_publishes_nothing`, `set_default_sets_and_clears_the_accounts_default`)
- Status: GUI shipped (M4, #0131), client-side over `mp_core::signatures`; another client's delete shows on the next open of the dialog, since the daemon publishes no `signature.removed`
- Note: there is no CLI equivalent, so the GUI takes this capability from the TUI.
  Every signature is a file under `signatures/`: the startup migration of #0107 turned the inline-text entries of `config.toml` into files, so none is edited through a temporary copy.

### ACC-11 Signature injection into a draft

- Classification: GUI parity
- Source anchor: the `{{SIGNATURE}}` marker handling in `src/send.rs:185-268`, the file and default lookup in `src/signatures.rs`
- Daemon surface: `draft.create`, `draft.reply`, `draft.forward`, and `draft.set_recipients` all re-splice
- GUI location: clients/desktop: `draft_create` and `draft_forward` pass the new-draft and forward wizards' `signature` or `no_signature`, and the daemon splices it into the draft; a reply carries the account's default (M3, #0131)
- Validation: `tests/draft_integration.rs`, `tests/daemon_gui_gaps.rs` (`a_reply_and_a_forward_carry_the_signature_they_name_or_none`), plus the marker assertions in `clients/tui/src/actions.rs` unit tests; `clients/desktop/src-tauri/src/commands.rs` (`validate_preview_and_signatures_answer_from_the_files`, `a_reply_and_a_forward_carry_the_signature_chosen_or_none`), `clients/desktop/src/components/compose/compose.test.tsx` (`the forward wizard picks a signature too, and none forwards without one`)
- Status: GUI shipped (M3, #0131) for a new draft and a forward; the desktop's `ce` rewrites the recipients and never re-splices, since its recipients dialog has no signature select, where the TUI's re-splices a changed signature
- Note: editing recipients re-splices the block, which is what makes this its own capability.

### ACC-12 Multi-account operation

- Classification: GUI parity
- Source anchor: the per-command account resolution and `--all-accounts` in `src/main.rs`
- Daemon surface: `account.list`, `state.bootstrap` per account, `state.event`
- GUI location: clients/desktop (M1, #0131 read slice)
- Validation: manual
- Status: not started
- Note: account switching preserves per-account list and selection state.

## Mailbox navigation and view switching

### MBX-01 List server mailboxes

- Classification: diagnostics and maintenance
- Source anchor: `mp list-mailboxes`, `src/main.rs`, `src/imap_client/mod.rs`
- Daemon surface: `mailbox.list_server`
- GUI location: not required (diagnostics and maintenance)
- Validation: manual, requires a live server; refusals and routing in `tests/daemon_sync_slice.rs`
- Status: routed (P4-U10); GUI not started
- Note: a live server call, distinct from the mailbox hierarchy the store already holds. The result carries `source` (`imap` or `graph`) because the two transports report different things about a mailbox: Graph's folder list has the item counts this listing prints and IMAP's `LIST` has the attributes and the delimiter instead.

### MBX-02 Browse and select mailboxes in the sidebar

- Classification: GUI parity
- Source anchor: TUI `j/k` and `Enter` (`clients/tui/src/app/keymap.rs:636`), `gm` (`clients/tui/src/app/keymap.rs:590`), `clients/tui/src/ui/sidebar.rs`
- Daemon surface: `state.bootstrap` mailbox summaries, `message.list` on selection
- GUI location: clients/desktop: the sidebar lists each account's mailboxes, `gm` focuses it, `j`/`k` move its cursor, and Enter or a click opens the mailbox (M1, #0129)
- Validation: TUI golden frames under `clients/tui/src/`; `clients/desktop/src/keymap/keymap.test.tsx` (`the sidebar cursor moves with j and Enter opens the mailbox`), `clients/desktop/src/components/shell/a11y.test.tsx` (`marks the selected mailbox as the current page`)
- Status: GUI shipped (M1, #0129)

### MBX-03 Jump to a mailbox by digit 1 through 9

- Classification: GUI parity
- Source anchor: `clients/tui/src/app/keymap.rs:558`
- Daemon surface: client-side over the bootstrap mailbox list, then `message.list`
- GUI location: clients/desktop: `1` to `9` open the selected account's nth mailbox (M1, #0129)
- Validation: TUI golden frames; `clients/desktop/src/keymap/keymap.test.tsx` (`digits jump to a mailbox of the selected account`)
- Status: GUI shipped (M1, #0129)

### MBX-04 Switch account

- Classification: GUI parity
- Source anchor: TUI `ga` (`clients/tui/src/app/keymap.rs:591`), guarded by `Guard::MultiAccount` so it appears only with more than one configured account
- Daemon surface: `account.list`, a second `state.bootstrap`
- GUI location: clients/desktop: `ga` selects the next account and opens its inbox, and a mailbox chosen in the sidebar selects the account it belongs to (M1, #0129)
- Validation: TUI golden frames; `clients/desktop/src/app/reducer.test.ts` (`keeps a choice made between a failed list_accounts and its retry`, `stays with the user's account across a later re-bootstrap`)
- Status: GUI shipped (M1, #0129)

### MBX-05 Switch between the Mail, Contacts, and Calendar views

- Classification: GUI parity
- Source anchor: TUI `Space m`, `Space c`, `Space a` (`clients/tui/src/app/keymap.rs:601-603`)
- Daemon surface: client-side; the view switch reads data already bootstrapped
- GUI location: clients/desktop: `Space m`, `Space c` and `Space a`, the sidebar's Contacts, Calendar and Settings entries and the palette's "Switch to Mail view", "Switch to Contacts view", "Switch to Calendar view" and "Open settings" switch between Mail and the full-pane views, which keep the selection, the marks and the outbox view; Settings is a fourth view with no key (M4, #0131; shell.md, "Views")
- Validation: TUI golden frames; `clients/desktop/src/keymap/keymap.test.tsx` (`Space c, Space a and Space m switch the view, and Escape comes back to Mail`, `a view's own key runs before any prefix arms, and the prefix arms again in Mail`), `clients/desktop/src/app/reducer.test.ts` (`switches to a view and back keeping the selection and the marks, with focus on the view's pane`), `clients/desktop/src/components/shell/a11y.test.tsx` (`makes the views sidebar buttons, one of them or a mailbox the current page, and names each view's region`)
- Status: GUI shipped (M4, #0131)

### MBX-06 Cycle pane focus and zoom the focused pane

- Classification: GUI parity
- Source anchor: TUI `Tab` (`clients/tui/src/app/keymap.rs:559`), `Shift+Tab` (`clients/tui/src/app/keymap.rs:560`), `z` (`clients/tui/src/app/keymap.rs:569`)
- Daemon surface: client-side
- GUI location: clients/desktop: `Tab` and `Shift+Tab` cycle sidebar, list and reader from the pane the model holds as focused, `gm` focuses the sidebar, `gr` and the palette's "Focus reader" the reader (a desktop key), and `z` zooms the focused list or reader; the focused pane carries `data-focused="true"`, drawn as a 1px `ring` line inside its edge (M1, #0129; shell.md, "Focus order")
- Validation: TUI golden frames; `clients/desktop/src/keymap/keymap.test.tsx` (`Tab and Shift+Tab cycle focus through sidebar, list and reader`, `marks the pane holding the focus with data-focused, and gr or the palette's Focus reader focus the reader`), `clients/desktop/src/app/reducer.test.ts` (`cycles focus through sidebar, list and reader and walks back through history`, `focus_reader focuses the reader from any pane, brings Mail back from a view, and cycles on from there`); zoom: `keymap.test.tsx` (`z zooms the focused list pane, hiding the reader`), `clients/desktop/src/components/reader/readerMode.test.tsx` (`z zooms the reader in text mode, and j scrolls the reader's container`)
- Status: GUI shipped (M1, #0129)
- Note: purely presentation state, so it never enters the canonical snapshot.

### MBX-07 Mailbox roles, slugs, sidebar labels, unread counts, and outbox badges

- Classification: GUI parity
- Source anchor: `clients/tui/src/ui/sidebar.rs` over the store's mailbox rows
- Daemon surface: `state.bootstrap`, then `state.event` for count changes
- GUI location: clients/desktop: the sidebar shows each mailbox's label and unread count, and the account's outbox line while a send is queued or failed (M1, #0129); the outbox line counts queued, failed and partly delivered rows from the outbox listing, moves on an `outbox:<account>` invalidation without a bootstrap, and is a button that opens the account's outbox (M3, #0131)
- Validation: TUI golden frames; `clients/desktop/src-tauri/src/commands.rs` (`mailboxes_carry_kind_and_totals`), `clients/desktop/src-tauri/src/fixture.rs` (`counts_are_computed_from_the_rows`), `clients/desktop/src/app/reducer.test.ts` (`marks the list and the counts stale on an invalidation of the selected mailbox`), `clients/desktop/src/components/outbox/outbox.test.tsx` (`opens from the sidebar line, which shows what waits, and lists the row with its notes`, `an invalidation refreshes the sidebar line without a bootstrap`)
- Status: GUI shipped (M1, #0129), and the outbox line's count and link (M3, #0131)
- Note: delivered through the bootstrap snapshot rather than a query.

## Listing, filtering, and search

### LST-01 List received messages offline, grouped by mailbox

- Classification: GUI parity
- Source anchor: `mp list-messages [--mailbox] [-n]`, `src/main.rs`, `list_mailbox` (`src/store/read.rs:178`)
- Daemon surface: `message.list`, one call per listed mailbox
- GUI location: clients/desktop: the list pane shows the selected mailbox, the Drafts mailbox as the draft listing, and reads it again on a `state.invalidate` (M1, #0129)
- Validation: `tests/cli_read_surface_integration.rs`, `tests/daemon_read_slice.rs`; `clients/desktop/src/app/events.test.tsx` (`refetches the shown list when its mailbox is invalidated`), `clients/desktop/src-tauri/src/commands.rs` (`the_drafts_mailbox_branches_to_the_draft_listing`), `clients/desktop/src-tauri/src/fixture.rs` (`listing_rows_decode_as_protocol_rows`)
- Status: routed (P4-U4); GUI shipped (M1, #0129)
- Note: the mailbox argument accepts a role, a slug, or the sidebar label, and the default lists every mailbox of the account. The name is resolved client-side against the configuration, so an unknown one is refused without a round trip and in the words it has always been refused in.

### LST-02 Navigate a list with per-item movement, top and bottom jumps, and half-page scrolling

- Classification: GUI parity
- Source anchor: TUI `j/k`, `gg/G`, `Ctrl+d`, `Ctrl+u` in the EMAIL LIST and BODY keymap sections (`clients/tui/src/app/keymap.rs`)
- Daemon surface: client-side over the list `message.list` returned
- GUI location: clients/desktop: `j`/`k` and the arrows, `gg`/`G`, `Ctrl+d`/`Ctrl+u` in the list, the reader following the selection (M1, #0129); the same keys on the reader scroll the body, inside the HTML frame through its bridge (PERSO-81)
- Validation: TUI golden frames; `clients/desktop/src/keymap/keymap.test.tsx` (`j and k move the selection in the list and the reader follows`, and the block `the reader frame's keys (PERSO-81)`), with no desktop test of the list's jumps or half-page keys
- Status: GUI shipped (M1, #0129)

### LST-03 Jump to a date in the list

- Classification: GUI parity
- Source anchor: TUI `gt` (`clients/tui/src/app/keymap.rs:652`), `clients/tui/src/app/jump_date.rs`
- Daemon surface: client-side while the list is whole; `message.jump_to_date` if paging lands
- GUI location: clients/desktop (M1, #0131 read slice)
- Validation: unit tests in `clients/tui/src/app/jump_date.rs`
- Status: not started
- Note: accepts relative expressions such as "last week" alongside absolute dates.

### LST-04 Filter the current list by metadata

- Classification: GUI parity
- Source anchor: TUI `fm` (`clients/tui/src/app/keymap.rs:624`)
- Daemon surface: client-side while the list is whole; `message.filter` if paging lands
- GUI location: clients/desktop (M1, #0131 read slice)
- Validation: TUI golden frames
- Status: not started

### LST-05 Toggle a flagged-only filter

- Classification: GUI parity
- Source anchor: TUI `fF` (`clients/tui/src/app/keymap.rs:673`)
- Daemon surface: client-side while the list is whole; `message.filter` if paging lands
- GUI location: clients/desktop (M1, #0131 read slice)
- Validation: TUI golden frames
- Status: not started

### LST-06 Search with one query grammar across every backend

- Classification: GUI parity
- Source anchor: `mp search [query] [--mailbox] [field flags] [-n] [--full]`, `SEARCH_LONG_ABOUT` (`src/main.rs:49`), `src/search.rs`, `src/imap_client/search.rs`
- Daemon surface: `message.search` as an `operation.*` for the server leg
- GUI location: clients/desktop (M1, #0131 read slice)
- Validation: unit tests in `src/search.rs`, `tests/cli_help_snapshot.rs` for the grammar's help text
- Status: the method exists (P5-U10c-I1), the CLI leg is not routed to it
- Note: the read slice (P4-U3/U4) contracted `mp search --local` and nothing else, so the server leg still opens its own IMAP session or Graph client in `src/main.rs`, and the plain-IMAP `has:attachment` post-filter still reads the local index there. It is the largest of the four groups in `CLI_ENGINE_RESIDUE`. `message.search_server` `{account, query, mailboxes?, limit?, exclude_message_ids?}` is served since P5-U10c-I1 and the TUI's overlay runs on it; three user-visible behaviours stand between this leg and it, which is why routing it is a unit of its own. `mp search` prints `Search in <mailbox> failed` to stderr per mailbox as it goes, where the operation reports `unreachable` at the settle; `mp search --mailbox` names the server mailbox directly, so a name the account does not configure is searched rather than refused; and the plain-IMAP `has:attachment` warning is a sentence about a post-filter the daemon now applies itself.
- Note: the grammar covers `from:`, `to:`, `cc:`, `subject:`, `body:`, `filename:`, `has:attachment`, `before:`, `after:` with `since:` as an alias, quoted phrases, `OR` groups, `in:`, and `message-id:`; `filename:` resolves only on Gmail, Exchange, or the local index.

### LST-07 Search the local ranked full-text index across every synced mailbox

- Classification: GUI parity
- Source anchor: `mp search --local`, `src/store/search.rs`, `tests/store_search_integration.rs`
- Daemon surface: `message.search`, whose params mirror the command's flags and whose hits come back in the store's ranking order
- GUI location: clients/desktop: Enter in the list header's field searches the selected account's store, and each hit carries its mailbox as a badge (M1, #0129)
- Validation: `tests/store_search_integration.rs`, `tests/daemon_read_slice.rs`; `clients/desktop/src/components/search/search.test.tsx` (`runs search_local on Enter and lists the hits with their mailbox`, `opens a hit from another mailbox in the reader`), `clients/desktop/src-tauri/src/commands.rs` (`local_search_flattens_the_row_beside_the_mailbox`)
- Status: routed (P4-U4); GUI shipped (M1, #0129)
- Note: `--body` is `body_query` on the wire, because `body` is already the `--full` switch; the client sends what the user typed and the daemon builds the query with `search::from_cli`, so one parser still serves every backend.

### LST-08 Merged search in the TUI

- Classification: GUI parity
- Source anchor: TUI `ff` (`clients/tui/src/app/keymap.rs:581`), `Action::ServerSearch` (`clients/tui/src/app/types.rs:1619`)
- Daemon surface: `message.search` local first, then `message.search_server` as a durable `operation.*` streaming hits on `state.event`
- GUI location: clients/desktop: Shift+Enter, `ff`, the header's server button and the palette's "Search server" run the server leg, which keeps the hits of a finished local search and excludes their Message-IDs; hits stream in until the settle, and Cancel or leaving the search cancels it (M1, #0129)
- Validation: TUI golden frames; `the_local_pass_finds_the_row_the_index_holds` (`src/tui_tests/commands.rs`); `clients/desktop/src/components/search/search.test.tsx` (the "server search" block), `clients/desktop/src-tauri/src/fixture.rs` (`a_server_search_streams_hits_and_finishes`), `clients/desktop/src-tauri/src/commands.rs` (`a_server_search_is_awaited_until_cancelled`)
- Status: routed (P5-U6 local, P5-U10c-I1 server); GUI shipped (M1, #0129)
- Note: `message.search_server`, deliberately not `message.list_server`, which P4-U10 gave to `mp fetch`'s one-mailbox query. The TUI's background thread is gone: the overlay appends each hit as its `message.server_hit` event arrives, matched by operation id because a fast retype leaves two searches in flight.
  Deduplication is by Message-ID and it is the daemon's: the client sends the Message-IDs the local pass is showing as `exclude_message_ids` and the settle counts them in `deduplicated`, so a message found twice appears once and the count is a fact any client reproduces.
  The local pass is `message.search` with `body: true` since P5-U6.
  The overlay holds a parsed query and the method takes what a user typed, so the query is rendered back into the grammar (`search::to_query_string`) rather than sent as an engine enum.
  A mailbox the server refuses lands in `unreachable` and does not fail the search; `LST-06`'s CLI leg is not routed through it yet, for the reasons that row gives.

### LST-09 Act on a search result without leaving the overlay

- Classification: GUI parity
- Source anchor: the SERVER SEARCH keymap section (`clients/tui/src/app/keymap.rs`): `Enter`, `e`, `y`, `f`, `r`, `R`, `w`, `a`, `b`, `o`, `O`
- Daemon surface: `message.get`, `message.materialise_html`, `message.materialise_attachment`, `message.fetch`, `message.archive`, `draft.reply`, `draft.forward`
- GUI location: clients/desktop (M1 for open and copy, #0129); archive is `a` on a result with a local row, which asks first like a list row and takes the hit out of the results (M2, #0131); a hit is a list row, so `r`, `ca` and `cf` reply and forward (a server-only hit through `draft_from_message`), `tb`, `to` and `ts` stand for the overlay's `b`, `o` and `O`, and `F` or the reader's Fetch button fetches a server-only hit into the store and opens its row (M3, #0131; shell.md, "Compose", and reader.md, "Drafts and server-only hits")
- Validation: TUI golden frames; the desktop's archive of a hit runs the list's path (`actionTargets` over the results), with no search-specific test; `clients/desktop/src/keymap/keymap.test.tsx` (`on a server-only hit, r, ca and cf build the draft from the hit's own headers`), `clients/desktop/src/components/reader/reader.test.tsx` (`F fetches it into the store, then the reader opens the row and a second F says it is there`, `the Fetch button fetches, and a refusal lands in the activity area`, `opens the hit's own markup in the browser, and one with none has no button`), `clients/desktop/src-tauri/src/commands.rs` (`fetch_ingests_a_server_only_message_once_and_then_says_it_is_present`)
- Status: routed (P5-U6, completed by P5-U10c-I1); GUI shipped (M2, #0131) for archive, and (M3, #0131) for reply, reply-all, forward, fetch, the browser rendition and attachments; the rest not started
- Note: a server-only hit has no row to archive, and `to` and `ts` on one say to fetch it first, since only a stored message has parts to materialise.
  The desktop fetches with `F`, because `f` is the find family's prefix there (`fm`, `ff`), and replies to all with `ca` rather than the overlay's `R`.
- Note: the overlay's reply, forward, archive, browser rendition and attachment keys are daemon methods since P5-U6; `f` and the three rendition keys joined them in P5-U10c-I1.
  `f` is `message.fetch` `{account, mailbox, message_id}`, a durable operation that is idempotent over a message the store already holds: it answers with that row and `already_present: true` and opens no session, so the overlay keeps its "Already in the local store" line by branching on a boolean rather than matching a refusal.
  `Enter` / `e` / `y` are `RD-06`'s `message.materialise_markdown`.
  One key still reads the store inline, and it is not this row's: `Action::OpenEventSource` on the agenda hands `$EDITOR` a row's `invite.ics` blob, which no `message.*` method serves.

### LST-10 Show the conversation a message belongs to

- Classification: GUI parity
- Source anchor: TUI `tt` (`clients/tui/src/app/keymap.rs:630`), the thread overlay in `clients/tui/src/ui/overlays.rs`
- Daemon surface: `message.thread`
- GUI location: clients/desktop (M1, #0131 read slice)
- Validation: TUI golden frames, `tests/daemon_thread_slice.rs`
- Status: routed (P5-U10d-I); GUI not started
- Note: `open_thread_overlay` asked `message.thread` on the session the `App` holds since P5-U10d-I, where it opened the store, read the row and folded `read::thread_messages` over it; it was the last read in `clients/tui/src/app/` that no method answered.
  The method takes `message.get`'s address and answers `{account, thread_id, subject, messages}`, the rows oldest first and one per `Message-ID`; a thread row carries the `mailbox` its copy lives in, which a listing row does not, because a conversation crosses mailboxes and the overlay's `Enter` switches to the one it opens.
  A message with no relatives answers with itself alone, and the client keeps its "No related emails for this message in the store" line by branching on the length.

### LST-11 Legacy server fetch with inline filters

- Classification: diagnostics and maintenance
- Source anchor: `mp fetch [--from --to --cc --subject --body --since --before -n --full --mailbox]`, `src/main.rs`
- Daemon surface: `message.list_server`
- GUI location: not required (diagnostics and maintenance)
- Validation: `tests/cli_help_snapshot.rs`
- Status: routed (P4-U10); GUI not started
- Note: superseded by `sync` plus `search`, kept because the migration preserves command surfaces; the deprecation decision is deferred to `BACKLOG.md` (`ANO-3`).

### LST-12 Dump message envelopes as NDJSON

- Classification: CLI automation
- Source anchor: `mp dump-mailbox --json [--mailbox ...]`, `src/dump.rs`, `docs/dump-allow-list.md`, `tests/dump_mailbox_integration.rs`
- Daemon surface: `message.list` with `projection: "envelope"`, whose output must stay byte-identical to the direct read
- GUI location: not required (CLI automation)
- Validation: `tests/dump_mailbox_integration.rs`, `tests/daemon_read_slice.rs`
- Status: routed (P4-U4); GUI not started
- Note: three contracts survive the move to an RPC data source: two runs over an unchanged store are byte-identical, `--json` stays required, and no filesystem path appears in the output. P4-U4 shipped the dump as a projection of `message.list` rather than the `message.dump` this row first proposed: the records answer the same question a listing does, one answer per account covers every selected mailbox in the dump's own sort order, and the client re-serialises `dump::EnvelopeRecord` with `dump::to_ndjson`, so the ordering contract and the field order stay in the module that owns them.

### LST-13 Coalesce queued input before repainting

- Classification: GUI parity
- Source anchor: `MAX_COALESCED_EVENTS` (`clients/tui/src/lib.rs:47`), `COALESCE_BUDGET` (`clients/tui/src/lib.rs:55`) and the drain in `clients/tui/src/lib.rs`, `poll_pending_event` (`clients/tui/src/event.rs:32`), `Action::suspends_terminal` (`clients/tui/src/app/types.rs:1716`)
- Daemon surface: client-side; the obligation is that a held key does not produce one round trip per repeat
- GUI location: clients/desktop (M1, #0129)
- Validation: unit tests in `clients/tui/src/lib.rs`
- Status: not started
- Note: event order is preserved, so leader keys and resizes are unaffected; the drain stops when the app is no longer running or an action hands the terminal to `$EDITOR`, whose GUI counterpart is the handoff into the Neovim PTY.

## Reading and rendering

### RD-01 Read one received message from the local store while offline

- Classification: GUI parity
- Source anchor: `mp show <selector> [--mailbox]`, `src/read_cmd.rs`, `tests/cli_read_surface_integration.rs`
- Daemon surface: `message.get`, addressed by `"<mailbox>/<uid>"` or by the selector the daemon resolves
- GUI location: clients/desktop: the reader pane shows the selected message's headers above its `message.html` rendition, loaded from the `mpmsg` scheme in a sandboxed frame whose only script is the app's bridge, admitted by a per-response nonce (M1, #0129; PERSO-81)
- Validation: `tests/cli_read_surface_integration.rs`, `tests/daemon_read_slice.rs`; `clients/desktop/src/components/reader/reader.test.tsx` (`loads the mpmsg URL in a sandbox that runs scripts but never on the app's origin, with no referrer`), `clients/desktop/src/components/reader/bridge.test.ts`, `clients/desktop/src-tauri/src/commands.rs` (`a_message_reads_as_text_and_as_meta`), `clients/desktop/src-tauri/src/reader.rs` (`the_path_parses_into_an_account_and_a_row`)
- Status: routed (P4-U4); GUI shipped (M1, #0129)
- Note: the selector crosses the socket unresolved, because resolving one needs the store the client no longer has; which account it names stays a client-side decision.

### RD-02 Emit one message as a single JSON object with headers, attachments, and body

- Classification: CLI automation
- Source anchor: `mp show --json`, `src/main.rs`, `src/read_cmd.rs`
- Daemon surface: `message.get`, whose result *is* this record
- GUI location: not required (CLI automation)
- Validation: `tests/cli_read_surface_integration.rs`, `tests/daemon_read_slice.rs`
- Status: routed (P4-U4); GUI not started
- Note: there is no second JSON projection. `message.get` returns `read_cmd::ShownMessage` field for field and `--json` prints it re-serialised, so the machine-facing answer cannot drift from the one the text layout renders.

### RD-03 Read HTML-dominant mail as the plain text flattened out of the markup

- Classification: GUI parity
- Source anchor: `wrap_and_style_body` (`clients/tui/src/ui/preview.rs:522`) over the body `parse::html_to_plain` (`src/parse.rs:199`) produced at ingest
- Daemon surface: `message.get` returns the flattened body
- GUI location: clients/desktop: the reader frame shows the `message.html` rendition, and a message without markup as escaped text (M1, #0129)
- Validation: unit tests in `clients/tui/src/ui/preview.rs`, `src/parse.rs`, `tests/daemon_read_slice.rs`; `clients/desktop/src-tauri/src/reader.rs` (`a_message_without_markup_is_served_as_escaped_text`), `clients/desktop/src/components/reader/reader.test.tsx` (`no longer fetches a plain-text body: the frame is the only path`)
- Status: routed for `mp show` (P4-U4); GUI shipped (M1, #0129)
- Note: #0111 retired the html2text rich render #0091 had added, so links, emphasis, tables, and lists arrive as a wrapped block and `b` / `tb` is the styled view.
  The GUI reader shows the `message.html` rendition in a sandboxed frame instead, and falls back to this plain text for a message without markup.

### RD-04 Headers pane with Bcc, Reply-To, the attachment marker, and clamped scrolling

- Classification: GUI parity
- Source anchor: `clients/tui/src/ui/headers.rs`, the HEADERS keymap section
- Daemon surface: `message.get` header block
- GUI location: clients/desktop (M1, #0131 read slice)
- Validation: unit tests in `clients/tui/src/ui/headers.rs`, TUI golden frames
- Status: not started

### RD-05 Inline image rendering in the reader

- Classification: GUI parity
- Source anchor: none; `clients/tui/src/images.rs` was deleted with the `ratatui-image` and `image` dependencies and the startup graphics-capability probe
- Daemon surface: `message.html`, whose rendition inlines `cid:` images as `data:` URIs
- GUI location: clients/desktop (M1, #0131 read slice), inside the sandboxed `message.html` frame
- Validation: `the_browser_rendition_inlines_cid_images_as_data_uris` (`src/tui_tests/actions_store.rs`) covers the inlining; no GUI test yet
- Status: retired from the TUI by #0109; GUI not started
- Note: `parse::inline_images` (`src/parse.rs:1078`) and `parse::embed_inline_images` (`src/parse.rs:1037`) remain, feeding the browser view, the `.html` companion and the `message.html` rendition the GUI reader loads.

### RD-06 Open a message read-only in `$EDITOR` as a Markdown rendition

- Classification: GUI parity
- Source anchor: TUI `Enter / e` (`clients/tui/src/app/keymap.rs:611`), search overlay `e`
- Daemon surface: `message.materialise_markdown` `{account, row_id|id|selector, mailbox?}`, the third member of the handle family
- GUI location: clients/desktop (M3, #0131)
- Validation: TUI golden frames
- Status: routed (P5-U10c-I1); GUI not started
- Note: the handle keeps its blob alive until release or expiry (`ANO-6`), and the file is written 0444 so `$EDITOR` opens it read-only (#0075).
  The rendition is released when the editor exits, where the pre-daemon file was unlinked; a second open of the same row is a second handle in a directory of its own, so the mode the first one left cannot stop it.
  `message.materialise_html` is not it: that renders the sender's markup for a browser, where this renders the store's own Markdown view of a message.
  The store keeps no `.md` per message, so the rendition is `store::read::render_markdown` on every call, which is what makes a handle the right shape rather than a path.
  It is the one member of the family that takes `row_id`, because the TUI holds a `MessageRef` and nothing else (#0050).

### RD-07 Copy a message's `mp://` selector to the clipboard

- Classification: GUI parity
- Source anchor: TUI `y` (`clients/tui/src/app/keymap.rs:618`)
- Daemon surface: `selector` on the `message.list` row and on the `message.search` hit, then a client-side clipboard write
- GUI location: clients/desktop: `y` copies the selected message's `mp://` selector and says "Copied <selector>" (M1, #0129); the copy goes through `copyText`, and the reader's Copy menu and the palette's "Copy link (mp://)" copy the same selector (M4, #0131; reader.md, "The toolbar")
- Validation: `tests/cli_selector_contract.rs` for the selector shape; `clients/desktop/src/lib/clipboard.test.ts` (`writes the text and says what it copied`), `clients/desktop/src/components/activity/activity.test.tsx` (`the Copy menu copies the sender's address, the mp:// link and the subject`); no test presses `y` itself
- Status: routed (P5-U10c-I1); GUI shipped (M1, #0129)
- Note: the row carries it rather than a `message.selector` query answering it, because the daemon already had the string in hand when it built the row.
  The TUI's `EmailEntry` carries the daemon's string, so `y` costs neither a round trip nor a store read; a parse-skipped draft and a server-only hit carry `None`, which are the two rows with no name to copy.
  `message.get` would answer it too, at the price of a whole-message read per clipboard copy.
  The cost is one key per row: P6-U10 measured a warm listing of 5 000 rows at 94 ms with fourteen keys, and this is the fifteenth.
  It is rendered daemon-side, so `tests/cli_selector_contract.rs`'s spelling stays the only one in the tree.

### RD-08 Copy the Markdown rendition path of a search hit

- Classification: GUI parity
- Source anchor: the search overlay `y`, the action set in `clients/tui/src/app/types.rs`
- Daemon surface: `message.materialise_markdown`, then a client-side clipboard write
- GUI location: clients/desktop, not built in M1 to M4: the palette lists the search overlay's "Copy the Markdown rendition path" with the badge "later", and the desktop calls no `message.materialise_markdown`
- Validation: TUI golden frames
- Status: routed (P5-U10c-I1); GUI not started
- Note: the path a `y` copies now names a file inside a handle directory, which the family releases after ten minutes: nothing reads a yanked path back, so what changed is how long a pasted one resolves.

## Selection and message actions

### MSG-01 Archive a received message on the server and locally

- Classification: GUI parity
- Source anchor: `mp archive <selector> [--mailbox]` (`src/main.rs`), TUI `a` (`clients/tui/src/app/keymap.rs:613`)
- Daemon surface: `message.archive`, addressed by `row_id`, by `"<mailbox>/<uid>"` or by the selector the daemon resolves; with `settle` (the default) the daemon commits the row move and drains the owed server op before it answers, and with `settle: false`, the TUI's contract (P5-U6), it queues the pair and answers, and the account's drainer drains it once mutations have been quiet for 1.5 s (#0133)
- GUI location: clients/desktop: `a` in the list or the reader, the palette's "Archive" and the reader toolbar's Archive, each asking "Archive this email?" first; the row leaves the list at once and comes back on a refusal or a rollback (M2, #0131)
- Validation: `tests/cli_selector_contract.rs`, `tests/daemon_mutation_slice.rs`, TUI golden frames; `clients/desktop/src/keymap/keymap.test.tsx` (`a asks first, and y archives the cursor row`), `clients/desktop/src/app/events.test.tsx` (`archives a row, and the drain's invalidation keeps it gone`), `clients/desktop/src-tauri/src/commands.rs` (`an_archive_moves_every_row_in_order_and_queues`)
- Status: routed (P4-U8, TUI P5-U6); GUI shipped (M2, #0131)
- Note: over an account with no credentials the backend refuses before the store is touched, which is the half the fixture reaches; the successful drain and its rollback wait on a fake IMAP backend.
  The desktop's rollback path is covered against the fixture's `rollback` simulation only, for the same reason.

### MSG-02 Delete a received message or a local draft

- Classification: GUI parity
- Source anchor: `mp delete <selector> [--mailbox] [--force]`, `mp delete --sent` (`src/main.rs`), TUI `d`
- Daemon surface: `message.delete` for received mail, `draft.discard` for a draft and for the `--sent` sweep, which is a parameter of the same method rather than one of its own
- GUI location: clients/desktop: `d`, the palette's "Delete" and the reader toolbar's Delete, after "Delete this email?"; a draft row is discarded through `draft_discard`, and a batch holding messages and drafts calls both (M2, #0131)
- Validation: `tests/draft_integration.rs`, `tests/daemon_mutation_slice.rs`; `clients/desktop/src/keymap/keymap.test.tsx` (`v marks and steps, and d with Enter deletes the marked rows in list order`, `d on a draft discards it after the confirmation`), `clients/desktop/src/app/events.test.tsx` (`discards a draft, and the state.remove that follows reloads the list without it`), `clients/desktop/src-tauri/src/fixture.rs` (`a_discarded_draft_is_removed_and_an_approved_one_refused`)
- Status: routed (P4-U8, TUI P5-U6); GUI shipped (M2, #0131)
- Note: `--force` is required to delete an approved draft because that is a queued send, and `--sent` clears every sent draft of the account and takes no selector.
  The desktop never forces, so an approved draft is refused and put back, and it has no `--sent` sweep.

### MSG-03 Toggle read and unread

- Classification: GUI parity
- Source anchor: TUI `u` (`clients/tui/src/app/keymap.rs:615`)
- Daemon surface: `message.set_read` `{account, row_id, read, settle}`
- GUI location: clients/desktop: `u` in the list or the reader, the row's "Unread" toggle, the reader toolbar and the palette; over marks, marking read wins when any marked row is unread (M2, #0131)
- Validation: `src/tui_tests/actions.rs`, TUI golden frames; `clients/desktop/src/components/mutations/mutation-ui.test.tsx` (`the flag and unread toggles act on their own row`), `clients/desktop/src/app/reducer.test.ts` (`marks read in place and moves the unread count`), `clients/desktop/src-tauri/src/commands.rs` (`flag_and_read_set_the_state_they_name`)
- Status: routed (P5-U6); GUI shipped (M2, #0131)
- Note: the daemon takes the new state rather than a toggle, and the TUI sends `settle: false`, so the row change and the owed `SetRead` commit together (#0039) and the account's drainer drains them once mutations have been quiet for 1.5 s (#0133).

### MSG-04 Toggle the `\Flagged` star

- Classification: GUI parity
- Source anchor: TUI `*` (`clients/tui/src/app/keymap.rs:616`)
- Daemon surface: `message.set_flag` `{account, row_id, flagged, settle}`
- GUI location: clients/desktop: `*` in the list or the reader, the row's "Flagged" toggle, the reader toolbar and the palette (M2, #0131)
- Validation: `src/tui_tests/actions.rs`, TUI golden frames; `clients/desktop/src/keymap/keymap.test.tsx` (`* flags the cursor row and unflags a flagged one`, `over marks, * flags them all when any is unflagged, and the marks clear`), `clients/desktop/src/app/reducer.test.ts` (`flags in place, and a refusal puts the old flag back`)
- Status: routed (P5-U6); GUI shipped (M2, #0131)
- Note: on a batch, flagging wins whenever any selected message is unflagged; the decision stays client-side, because it is a property of the selection the user can see.
- Note: flagging leaves the read bit alone, which is what a shared "set flags" method would get wrong.

### MSG-05 Move a message to another mailbox through a fuzzy picker

- Classification: GUI parity
- Source anchor: TUI `M`, `Action::MoveToMailbox` (`clients/tui/src/app/types.rs:1571`)
- Daemon surface: `message.move` `{account, row_id, destination, settle}`, plus the mailbox list from `state.bootstrap`
- GUI location: clients/desktop: `M`, the reader toolbar's Move and the palette open a picker of the account's mailboxes less Drafts and the rows' own mailbox, filtered as the name is typed; from Drafts it says "Quick-move is not available in this mailbox", the TUI's words (M2, #0131)
- Validation: `src/tui_tests/actions.rs`, TUI golden frames; `clients/desktop/src/keymap/keymap.test.tsx` (`M opens the mailbox picker, typing filters it and Enter moves`), `clients/desktop/src/components/mutations/mutation-ui.test.tsx` (`filters as the name is typed, and Escape cancels`, `says why a Drafts row cannot move`), `clients/desktop/src-tauri/src/commands.rs` (`a_move_lands_in_the_destination_by_slug_or_label`)
- Status: routed (P5-U6); GUI shipped (M2, #0131)
- Note: `destination` is a role, slug or sidebar label, resolved by the daemon; the client no longer checks the sidebar's `server_name`, because `find_server_name_for_role` is the same mapping read on the side that owns it.
  In the desktop a moved row leaves its list at once but shows in a destination list only after the drain's `state.invalidate` re-reads it.

### MSG-06 Multi-select and batch actions

- Classification: GUI parity
- Source anchor: TUI `v` (`clients/tui/src/app/keymap.rs:653`), `Ctrl+a` (`clients/tui/src/app/keymap.rs:654`), the batch actions in `clients/tui/src/app/types.rs`
- Daemon surface: one call per selected message, in the selection's order, over `message.set_read`, `message.set_flag`, `message.archive`, `message.delete`, `draft.discard`, `draft.approve` and `draft.demote`; selection stays client-side
- GUI location: clients/desktop: `v` marks the cursor row and steps, `Ctrl+a` marks every shown row, Shift+click marks a range and Cmd+click or the row's mark box one row, Escape clears the marks, and a hidden "N marked" status counts them; keys and the palette act on the marks in list order (M2, #0131)
- Validation: `src/tui_tests/actions.rs`, TUI golden frames; `clients/desktop/src/components/mutations/mutation-ui.test.tsx` (`marks rows by the box, Shift+click and Cmd+click, counts them and clears them`), `clients/desktop/src/keymap/keymap.test.tsx` (`Ctrl+a marks every row, and Escape clears the marks before the selection`), `clients/desktop/src/app/reducer.test.ts` (the multi-select cases), `clients/desktop/src-tauri/src/commands.rs` (`an_unknown_row_fails_alone_and_the_rest_go_ahead`, `a_batch_stops_at_an_error_about_the_whole_batch`)
- Status: routed (P5-U6); GUI shipped (M2, #0131)
- Note: the plain form rather than the plural address this row sketched: a reference to a row that is gone is skipped with a log line while the rest of the selection proceeds, which one call per row gives for free and a plural address would have to re-invent as a partial-failure shape. A plural address is worth taking the day a selection's round trips show up in a measurement.
  The desktop keeps that shape: one Tauri command per account carries the row ids, the Rust layer sends one daemon call per row in order with `settle: false`, a `-32602` refusal fails its row alone, and an error about the whole batch stops it.
  List windowing is off, so marking every row of a large mailbox mounts every row.

### MSG-07 Confirmation dialogs guarding destructive actions

- Classification: GUI parity
- Source anchor: the confirm variants in `clients/tui/src/app/types.rs`, covering approve, demote, archive, delete, send, send-approved, and signature deletion
- Daemon surface: client-side, over the same methods
- GUI location: clients/desktop: archive and delete ask first in a dialog ("Archive this email?" with the sender and subject, "Delete 3 emails?" over marks), `y` or Enter confirms and `n` or Escape cancels (M2, #0131); the same dialog asks "Approve 2 drafts?" and "Mark 2 drafts as draft?" over marks, "Draft is not approved. Approve and send?" or "Send this email?" before `x`, "Send all approved emails?" before `cX`, and before an outbox retry or discard, with a warning line (M3, #0131; shell.md, "Approve and demote", "Send" and "Retry and discard"); the Signatures dialog's `d` raises it over the dialog as "Delete signature '<name>'?" with the file's path (M4, #0131; shell.md, "Signatures")
- Validation: TUI golden frames; `clients/desktop/src/keymap/keymap.test.tsx` (`a asks first, and y archives the cursor row`, `n cancels the confirmation and nothing is called`, `cA over marked drafts asks first, with the TUI's words, and approves the batch`, `x on a draft asks the TUI's approve-and-send question, and y sends it with the hold`, `x on received mail says it needs a draft, and n cancels a send`), `clients/desktop/src/components/mutations/mutation-ui.test.tsx` (`Archive asks first and archives on confirm`, `Delete asks first, and Cancel deletes nothing`), `clients/desktop/src/components/outbox/outbox.test.tsx` (`R retries the cursor row after the warning, and the settle says how it ended`), `clients/desktop/src/components/signatures/signatures.test.tsx` (`d asks first with the file's path; n keeps it, y deletes it`)
- Status: GUI shipped (M2, #0131) for archive and delete, (M3, #0131) for approve, demote, send and send-approved, and (M4, #0131) for signature deletion
- Note: archive asks for confirmation in the desktop as the TUI does, since the daemon has no undo for it.

### MSG-08 Mark a message read on an explicit open

- Classification: GUI parity
- Source anchor: `mark_open_read` (`clients/tui/src/commands.rs:1445`), reached from the received-row branch of `Action::EditCurrent` and from `Action::MarkAsRead`, which `queue_mark_open_read` (defined at `clients/tui/src/app/mod.rs:1230`, pushed from `clients/tui/src/app/keys.rs:322` and `:336`) queues on a focus move into the body pane
- Daemon surface: `message.set_read` carrying the opened `MessageRef` as `row_id`
- GUI location: clients/desktop: Enter on a row, a double-click on a row or a search hit, and Tab or Shift+Tab landing in the reader each mark an unread message read once; a cursor move or a single click marks nothing (M2, #0131)
- Validation: `clients/tui/src/commands.rs` unit tests, `src/tui_tests/actions.rs`; `clients/desktop/src/components/mutations/mutation-ui.test.tsx` (`moving the cursor marks nothing; Enter on an unread row marks it read once`, `Tab into the reader and a double-click open an unread row and mark it read`)
- Status: routed (P5-U6); GUI shipped (M2, #0131)
- Note: #0110 retired the #0087 trigger that fired on every cursor move, so walking the list marks nothing and the GUI marks on the open rather than on selection; the action carries the `MessageRef` the open resolved, so a coalesced key batch marks the row that was opened.

### MSG-09 Optimistic local mutation state reconciled against the server

- Classification: GUI parity
- Source anchor: `src/pending_ops.rs`, `src/ops.rs`
- Daemon surface: `state.event` carrying pending and reconciled states
- GUI location: clients/desktop: every change applies at once and keeps one saved state per axis of the row (`clients/desktop/src/app/pending.ts`), a row in flight is `aria-busy` with a pending mark, and a refusal, a thrown command or a `mutations.rolled_back` puts the saved state back with a notice in the activity area (M2, #0131)
- Validation: unit tests in `src/pending_ops.rs`, `src/ops.rs`; `clients/desktop/src/app/reducer.test.ts` (the "mutations and pending state" block), `clients/desktop/src/app/mutations.test.ts`, `clients/desktop/src/app/events.test.tsx` (`drops a list read that started before the archive and landed after it`, `brings rows back on a rollback and says so`)
- Status: GUI shipped (M2, #0131) as a client-side model; the daemon publishes no per-row pending state
- Note: a mutation publishes no event of its own, and `mutations.rolled_back` names a count per account and no row, so the desktop puts back every row of that account still pending and re-reads its lists.
  A list generation per list, the TUI's `mailbox_load_generation`, drops a list read that started before a change, and every list answer is laid under the pending changes.
  The daemon has no undo for archive, delete or move, so the rollback notice is the only undo surface.
  Search hits streamed from the server are not laid under the pending changes.

## Drafts, composition, reply, and forward

### DFT-01 Create a draft from the template and print its selector

- Classification: GUI parity
- Source anchor: `mp new <name>` (`src/main.rs`), TUI `cn` (`clients/tui/src/app/keymap.rs:583`)
- Daemon surface: `draft.create`
- GUI location: clients/desktop: `cn` from any pane or the palette's "New draft" opens the wizard, whose submit calls `draft_create` with the recipients, the signature and any inline body, and opens the file in the editor unless a body was typed (M3, #0131; shell.md, "Compose", "The wizard")
- Validation: `tests/draft_integration.rs`, `tests/daemon_draft_slice.rs`; `clients/desktop/src/keymap/keymap.test.tsx` (`cn opens the new-draft wizard for the shown account`), `clients/desktop/src/components/compose/compose.test.tsx` (`Enter moves to the next field, and Cmd+Enter creates the draft and opens it in the editor`, `shows the daemon's refusal in the dialog, which stays open`), `clients/desktop/src-tauri/src/commands.rs` (`a_created_draft_is_a_file_with_frontmatter_and_a_taken_name_is_refused`)
- Status: routed (P4-U6); GUI shipped (M3, #0131)
- Note: the desktop prints no selector; the new draft's file opens in the editor and its row appears in Drafts through `draft.changed`.

### DFT-02 List the account's drafts, optionally filtered by status

- Classification: GUI parity
- Source anchor: `mp list [--status]`, `src/main.rs`
- Daemon surface: `draft.list`
- GUI location: clients/desktop: the Drafts mailbox lists every draft with its status pill (`draft`, `approved`), a pencil badge while the editor has it, a "sending" badge while a send runs, and a file that does not parse as an `invalid` row named by its stem (M3, #0131; shell.md, "Draft rows")
- Validation: `tests/draft_integration.rs`, `tests/daemon_draft_slice.rs`; `clients/desktop/src/app/events.test.tsx` (`a draft.invalid from the editor turns the row invalid and keeps it selected`), `clients/desktop/src/components/compose/compose.test.tsx` (`ends when the draft is removed, and shows the editing mark on its row until then`), `clients/desktop/src-tauri/src/fixture.rs` (`the_seeded_drafts_are_files_with_frontmatter_in_a_per_run_dir`)
- Status: routed (P4-U6); GUI shipped (M3, #0131) for the listing with each draft's status; there is no status filter

### DFT-03 Validate draft frontmatter

- Classification: GUI parity
- Source anchor: `mp validate [selector]`, `src/draft.rs`
- Daemon surface: `draft.validate`
- GUI location: clients/desktop: the draft preview in the reader shows `draft_validate`'s report, "Valid" or "Not sendable" with the error and the warnings, and `send_draft` validates before it approves, so a draft that does not validate keeps its status (M3, #0131; reader.md, "Drafts and server-only hits", and shell.md, "Send")
- Validation: `tests/draft_integration.rs`, `tests/daemon_draft_slice.rs`, unit tests in `src/draft.rs`; `clients/desktop/src/components/compose/compose.test.tsx` (`shows the headers, the status, the validation and the body of the selected draft`), `clients/desktop/src-tauri/src/commands.rs` (`validate_preview_and_signatures_answer_from_the_files`, `a_draft_that_does_not_validate_keeps_its_status_and_is_not_sent`)
- Status: routed (P4-U6); GUI shipped (M3, #0131)
- Note: an invalid draft stays editable and cannot be approved or sent.

### DFT-04 Approve a draft and demote it back to draft status

- Classification: GUI parity
- Source anchor: `mp mark-approved`, `mp mark-draft` (`src/main.rs`), TUI `cA` and `cD` (`clients/tui/src/app/keymap.rs:667-668`)
- Daemon surface: `draft.approve`, `draft.demote`, resolved through `draft.path` first so the client knows the previous status
- GUI location: clients/desktop: `cA` and `cD` in Drafts, the palette and the draft preview's Approve and Back to draft buttons, on the marked drafts after a confirmation or on the cursor draft at once; the status changes in the list as a pending axis, and a draft that does not parse is refused alone with the diagnostic and the path (M3, #0131; shell.md, "Approve and demote")
- Validation: `tests/draft_integration.rs`, `tests/daemon_draft_slice.rs`; `clients/desktop/src/keymap/keymap.test.tsx` (`cA approves the cursor draft and cD puts it back`, `cA over marked drafts asks first, with the TUI's words, and approves the batch`), `clients/desktop/src/components/compose/compose.test.tsx` (`a draft that does not parse is refused, and the alert names why and where`), `clients/desktop/src-tauri/src/commands.rs` (`approve_refuses_an_invalid_draft_with_its_payload_and_goes_on`)
- Status: routed (P4-U6); GUI shipped (M3, #0131)

### DFT-05 Preview a draft as a dry run through a bare selector

- Classification: GUI parity
- Source anchor: the top-level positional `[SELECTOR]` argument in `src/main.rs`
- Daemon surface: `draft.preview`
- GUI location: clients/desktop: Enter on a draft shows `DraftPreview` in the reader, with From, To, Cc, Bcc, the status, the body cut at 500 characters and the file path (M3, #0131; reader.md, "Drafts and server-only hits")
- Validation: `tests/cli_selector_contract.rs`, `tests/mime_oracle_integration.rs`, `tests/daemon_draft_slice.rs`; `clients/desktop/src/components/compose/compose.test.tsx` (`Enter opens a draft's preview in the reader`, `shows the headers, the status, the validation and the body of the selected draft`)
- Status: routed (P4-U6); GUI shipped (M3, #0131)
- Note: the send confirmation shows the TUI's "To: <to> - <subject>" line rather than the dry run.

### DFT-06 Resolve a draft selector to its filesystem path

- Classification: CLI automation
- Source anchor: `mp path <selector>`, `src/main.rs`, `src/selector.rs`
- Daemon surface: `draft.path`
- GUI location: not required (CLI automation)
- Validation: `tests/cli_selector_contract.rs`, `tests/daemon_draft_slice.rs`
- Status: routed (P5-U6); the desktop resolves `e`, `ce` and the draft attachment commands through it (M3, #0131)
- Note: the only selector-to-path edge, and the handle external editors and agents use, so it stays supported under the filesystem boundary.
  It is also every draft-only key of the TUI since P5-U6: `cursor_draft` resolves the file under the cursor through it, where it used to open the store's drafts index.
  The lookup had two outcomes (not in the index, and the index could not be read) where `draft.path` has one refusal, so the second status line is gone and its reason is in the log.
  Since P5-U10b (#0126) it is also the Drafts preview: `App::draft_body` asks for the path and parses the file with `mp_core::draft::parse_email_draft`, where `load_draft_body` opened the store to look the id up. The body does not travel, because both ends read the file with the same parser; what the client cannot do is turn an `id:` into a path.

### DFT-07 Edit a draft in the editor

- Classification: GUI parity
- Source anchor: `mp edit <selector>`, `src/main.rs`
- Daemon surface: `draft.path`, then a client-side editor session on the canonical file
- GUI location: clients/desktop: `e` on a draft, the palette and the draft preview's Edit in editor run `draft_path`, then `editor_open`, which starts the resolved external editor without waiting for it; the editing banner names the draft and the editor, with Reopen in editor and Done (M3, #0131; shell.md, "The editing banner", rust-layer.md, "Drafts and the editor")
- Validation: `tests/daemon_draft_slice.rs`, with a stub editor that records the path it was handed; `clients/desktop/src/keymap/keymap.test.tsx` (`e on a draft resolves its path and opens the editor; on a message it opens the reader`), `clients/desktop/src/components/compose/compose.test.tsx` (`names the draft and the editor; Reopen runs the editor again and Done ends the session`, `an editor that did not start is a failure notice naming what to set`), `clients/desktop/src-tauri/src/editor.rs` (`the_env_and_the_setting_win_over_visual_editor_and_the_probes`, `a_terminal_editor_in_the_environment_falls_through_to_the_probes`)
- Status: routed (P4-U6); GUI shipped (M3, #0131) through the external editor, and (M5, #0130) through the embedded session for a terminal editor
- Note: a terminal editor (`nvim`, `vim`, `hx`, ...) named by `MP_DESKTOP_EDITOR`, the `editor` setting, `$VISUAL` or `$EDITOR` opens the draft in the embedded terminal pane on the same file (shell.md, "The embedded editor"; rust-layer.md, "Terminal sessions"); a GUI editor, a probed `code`, `zed`, `subl` or `cursor`, or `open -t` keeps the external route, in that order.

### DFT-08 Create a reply or a reply-all draft from a received message

- Classification: GUI parity
- Source anchor: `mp reply <selector> [--all] [--mailbox]` (`src/main.rs`), TUI `r`, `cr` (`clients/tui/src/app/keymap.rs:626`), `ca`, search overlay `r` and `R`
- Daemon surface: `draft.reply`, and `draft.create_from_message` for a hit with no local row
- GUI location: clients/desktop: `r` or `cr` and `ca` from the list or the reader, the reader toolbar's Reply and Reply all, and the palette, which open the new draft in the external editor; on a server-only hit they go through `draft_from_message` (M3, #0131; shell.md, "Compose")
- Validation: `tests/draft_integration.rs`, `tests/daemon_draft_slice.rs`, `tests/daemon_draft_from_message_slice.rs`; `clients/desktop/src/keymap/keymap.test.tsx` (`r and cr reply to the cursor row and open the draft in the editor, ca replies to all`, `on a server-only hit, r, ca and cf build the draft from the hit's own headers`), `clients/desktop/src/components/compose/compose.test.tsx` (`Reply, Reply all and Forward act on the open message`), `clients/desktop/src-tauri/src/commands.rs` (`a_reply_carries_in_reply_to_and_the_subject`)
- Status: routed (P5-U6, and P5-U10d-I for the hit with no row); GUI shipped (M3, #0131)
- Note: the TUI's four reply keys went through it in P5-U6, addressed by the `row_id` the method gained for them.
  A search hit that resolved to no local row is the one reply neither `draft.reply` nor `draft.forward` can build, because every form of their `source` is an address into the store; P5-U10d-I routed it through `draft.create_from_message` `{account, kind, message}`, where the message travels instead of an address, nothing is ingested, no unread count moves, and a hit with no `Message-ID` or in a mailbox the sidebar does not list still quotes.

### DFT-09 Forward a message to new recipients

- Classification: GUI parity
- Source anchor: `mp forward <selector> [--mailbox]` (`src/main.rs`), TUI `cf`, search overlay `w`
- Daemon surface: `draft.forward`, and `draft.create_from_message` for a hit with no local row
- GUI location: clients/desktop: `cf` from the list or the reader, the reader toolbar's Forward and the palette open the forward wizard with To, Cc, Bcc and a `Fwd: ` subject, whose submit calls `draft_forward` with `headers` and opens the draft in the external editor; a server-only hit is forwarded at once through `draft_from_message`, and the search overlay's `w` is `cf` on the hit's row (M3, #0131; shell.md, "The wizard")
- Validation: `tests/draft_integration.rs`, `tests/mime_oracle_integration.rs`, `tests/daemon_draft_slice.rs`, `tests/daemon_draft_from_message_slice.rs`; `clients/desktop/src/keymap/keymap.test.tsx` (`cf on a stored message asks for the recipients first, with the forward's subject`), `clients/desktop/src-tauri/src/commands.rs` (`a_forward_carries_the_attachments_and_a_hit_is_built_from_itself`)
- Status: routed (P5-U6, and P5-U10d-I for the hit with no row); GUI shipped (M3, #0131)
- Note: the forward carries the original attachments, which the GUI must reproduce rather than dropping.
  A forward of a server-only hit is the exception and carries none: `message.search_server` streams the envelope and the two body renditions, so since P5-U10c-I1 the client has no parts to forward, and `draft.create_from_message` takes none.
  P5-U6 routed the TUI's two forward keys through it and gave the method `headers`, the compose wizard's override of the recipients and the subject it collected before the draft existed.

### DFT-10 Compose wizard for new and forwarded mail

- Classification: GUI parity
- Source anchor: the compose wizard variants in `clients/tui/src/app/types.rs`, `clients/tui/src/ui/compose.rs`
- Daemon surface: `draft.create`, `draft.forward` with `headers`, `signature.list`; the wizard itself is client-side
- GUI location: clients/desktop: `ComposeWizard`, a dialog opened by `cn` ("New draft") and `cf` ("Forward"), with To, Cc, Bcc and Subject, a Signature select, an inline Body for a new draft, which skips the editor when it is filled, as in the TUI, Enter to the next field, Cmd+Enter or Ctrl+Enter to submit and Escape to cancel; it needs one recipient, as the TUI's does (M3, #0131; shell.md, "The wizard")
- Validation: TUI golden frames; `clients/desktop/src/components/compose/compose.test.tsx` (`has the TUI's fields, the default signature preselected, and a none option`, `Enter moves to the next field, and Cmd+Enter creates the draft and opens it in the editor`, `refuses a draft with no recipient, as the TUI does, and stays open`, `Escape cancels and writes nothing`), `clients/desktop/src/components/shell/a11y.test.tsx` (`names the new-draft wizard, starts it in To, and keeps Tab inside it`)
- Status: routed (P5-U6) for its forward mode; GUI shipped (M3, #0131), with the inline body since `draft.create` took `body` and `headers` (#0131, "Daemon gaps closed")
- Note: an inline body field, a signature picker, and a submit chord; the overlay-internal keys go into `docs/baselines/pre-daemon/manual-keys.md`, the P0-U2 inventory (`ANO-2`).

### DFT-11 Edit the recipients of an existing draft

- Classification: GUI parity
- Source anchor: TUI `ce` in the drafts mailbox (`clients/tui/src/app/keymap.rs:656`)
- Daemon surface: `draft.set_recipients`, which re-splices the signature block
- GUI location: clients/desktop: `ce` in Drafts, the palette and the draft preview's Edit recipients open the "Edit recipients" dialog filled from the listed draft row, Bcc included, and its submit calls `draft_set_recipients`, which rewrites the `to`, `cc`, `bcc` and `subject` lines client-side with `mp_core::draft::rewrite_draft_recipients` (M3, #0131; shell.md, "The recipients dialog")
- Validation: TUI golden frames; `edit_recipients_finds_the_draft_through_the_index` (`src/tui_tests/actions_store.rs`); `clients/desktop/src/keymap/keymap.test.tsx` (`ce opens the recipients dialog filled from the listed draft, Bcc included`), `clients/desktop/src/components/compose/compose.test.tsx` (`rewrites the recipients and keeps the subject when it did not change`), `clients/desktop/src-tauri/src/commands.rs` (`set_recipients_rewrites_the_header_and_keeps_the_body`)
- Status: GUI shipped (M3, #0131) for the recipients and the subject; the dialog has no signature select, so it never re-splices the signature the TUI's `ce` re-splices when it changed
- Note: `draft.set_recipients` is not built and the rewrite is still a client-side write to the file `draft.path` resolved (P5-U6 routed the resolution, not the write).
  It reaches no engine module, so the residue gate does not name it; what it costs is a second drafts-index refresh the daemon could have done in one.

### DFT-12 Watch draft files and refresh the derived index after an external edit

- Classification: GUI parity
- Source anchor: implicit workflow, no command; the drafts index refresh in `clients/tui/src/`
- Daemon surface: daemon-owned watcher emitting `state.event`, plus `draft.list`'s fresh directory scan
- GUI location: clients/desktop: a save in the editor reaches the Drafts list as `draft.changed`, which re-reads it with the editing banner kept, and a save that breaks the frontmatter as `draft.invalid`, which turns the row `invalid` and keeps it selected; no action reloads anything itself (M3, #0131; shell.md, "Compose")
- Validation: `tests/daemon_draft_watch.rs`, `tests/daemon_draft_index_slice.rs`; `clients/desktop/src/app/events.test.tsx` (`a draft.changed from the editor re-reads the Drafts list and keeps the editing banner`, `a draft.invalid from the editor turns the row invalid and keeps it selected`), `clients/desktop/src-tauri/src/commands.rs` (`an_editor_save_publishes_draft_changed_and_bumps_the_row`), `clients/desktop/src-tauri/src/fixture.rs` (`a_broken_draft_is_published_invalid_skipped_and_refused`)
- Status: routed (P3b-U10 served it, P5-U10d-I dropped the client's poll); GUI shipped (M3, #0131)
- Note: the mechanism that keeps the GUI correct while Neovim writes the file.
  The daemon has watched every configured account's drafts directory since P3b-U10 and `draft.list` answers from a directory scan, so the client's one-second fingerprint poll and its three `store::drafts::refresh_account` calls needed no method to replace them; P5-U10d-I deleted all four and reduced `draft.changed`, `draft.invalid` and `state.remove` of a `draft:` resource into the invalidate-and-reload the poll made.
  The store's `drafts` table survives as the daemon's own, refreshed on every `mailbox.list` for the sidebar count, which is the one thing that reads it.

## Attachments

### ATT-01 Open a received message's attachment in the default application

- Classification: GUI parity
- Source anchor: `mp open <selector> [--mailbox]` (`src/main.rs`), TUI `to`, search overlay `o`
- Daemon surface: `message.materialise_attachment`, one call per part, opened client-side through `parse::open_file_with_system`
- GUI location: clients/desktop: `to` from the list or the reader opens a message's only part at once and picks among several in the "Open attachment" dialog, and each entry of the reader's attachment list has an Open button; `attachment_open` hands the daemon's file to the system opener and says "Opened: <name>" (M3, #0131; reader.md, "Attachments")
- Validation: `tests/cli_selector_contract.rs`, `tests/daemon_mutation_slice.rs`; `the_cursor_row_materialises_its_blobs_into_daemon_handles` (`src/tui_tests/actions_store.rs`); `clients/desktop/src/components/reader/reader.test.tsx` (`opens a part with the system opener and saves one into the directory the dialog names`, `to opens one attachment at once and picks among several; ts saves the checked parts`, `says No attachments for a message without any`), `clients/desktop/src-tauri/src/attachments.rs` (`open_materialises_the_part_and_hands_the_daemons_file_to_the_opener`)
- Status: routed (P5-U6); GUI shipped (M3, #0131)
- Note: the printed path is the one row of the slice that is not byte-identical to the pre-daemon binary and cannot be: a materialised file lives under `<data_dir>/runtime/handles/<handle>/` with a lifetime attached, rather than in the client's own temp directory. The handle is deliberately not released, because the viewer just launched is holding the file.

### ATT-02 Save a received message's attachments into a client-named directory

- Classification: GUI parity
- Source anchor: `mp save <selector> [-o dir] [--mailbox]`, the option at `src/main.rs:301` and the handler in `src/main.rs`
- Daemon surface: `message.materialise_attachment`, one call per part, plus a client-side copy into the destination and a `message.release_handle` per part
- GUI location: clients/desktop: `ts` from the list or the reader, or a Save button in the reader's attachment list, opens the Save dialog with a checkbox per part and a Directory field that starts at `~/Downloads` and then offers the last directory used; `attachment_save` copies with the `_1` rule and releases each handle (M3, #0131; reader.md, "Attachments")
- Validation: `tests/cli_selector_contract.rs`, `tests/daemon_mutation_slice.rs`; `clients/desktop/src/components/reader/reader.test.tsx` (`opens a part with the system opener and saves one into the directory the dialog names`, `Browse picks the directory natively, written with ~, and a closed or missing picker leaves the field`, `keeps the Save dialog open on a relative directory, and offers the last directory used next`), `clients/desktop/src-tauri/src/attachments.rs` (`save_applies_the_underscore_rule_and_releases_each_handle`, `the_save_directory_expands_home_and_refuses_a_relative_path_or_a_file`)
- Status: routed (P4-U8); GUI shipped (M3, #0131) with a typed directory, which Browse fills from the native folder picker (`tauri-plugin-dialog`); `ts` on a draft saves nothing and says its files are already on disk
- Note: the destination defaults to the current directory and only the client knows what that means (`ANO-15`), so the client resolves it twice over: the absolute form anchors the writes, and the spelling the user typed is what the `✓` lines print. The result is a permanent user artifact rather than a daemon-owned handle with a lifetime, which is what separates this entry from `ATT-01`, `ATT-04`, and `ATT-05`. The daemon never renames a part, so the `_1` rule for two parts sharing a name is applied client-side, within one call.

### ATT-03 Attach a file to a draft

- Classification: GUI parity
- Source anchor: TUI `ta` in the drafts mailbox (`clients/tui/src/app/keymap.rs:659`), `resolve_attachment_paths` (`src/send.rs:1964`)
- Daemon surface: `draft.attach` with an absolute path
- GUI location: clients/desktop: `ta` in Drafts, the palette or the draft preview's Attach file open the "Attach file" dialog, whose File field takes an absolute or `~` path; `draft_attach` appends it through the daemon's `draft.attach`, and the preview's attachment list removes an entry with `draft_attachment_remove` over `draft.detach` (M3, #0131; reader.md, "Drafts and server-only hits")
- Validation: `tests/draft_integration.rs`; `clients/desktop/src/components/compose/compose.test.tsx` (`ta opens the path dialog, which keeps a missing file open and attaches an existing one`, `Browse attaches a file the native picker chose, under ~ as the draft keeps it, and a path outside home as picked`, `the preview lists the entries in order, opens one, flags a missing one and removes one`), `clients/desktop/src-tauri/src/attachments.rs` (`attach_appends_in_order_refuses_duplicates_and_missing_files_and_remove_rewrites`, `a_draft_entry_resolves_as_the_send_path_resolves_it`)
- Status: GUI shipped (M3, #0131) with a typed path, which Browse fills from the native file picker (`tauri-plugin-dialog`), under the home directory as `~/…`
- Note: appends to the `attachments:` frontmatter list and verifies the path at the prompt, so the GUI file picker applies the same verification.
  The daemon serves `draft.attach`, `draft.attachments` and `draft.detach` since #0131 ("Daemon gaps closed"), which the desktop calls; the TUI still rewrites the file itself.
- Accepted divergence (P4-U15 review): a relative entry resolves against **the draft file's own directory**, where the pre-daemon binary resolved it against the sending process's working directory. No attachment path crosses the wire - `send.draft` carries a selector and the daemon reads the draft itself - so the client has nothing to rewrite, and the daemon's cwd is whichever directory happened to start it (`daemon::lifecycle::spawn_detached` sets no `current_dir`). The draft's directory is the one anchor both processes agree on; `send::resolve_attachment_paths` takes it explicitly and reads no `current_dir()`. Pinned by `tests/daemon_send_attachments.rs` and the unit rows in `src/send.rs` and `tests/daemon_autostart.rs`. A `~`-relative or absolute entry is unaffected, which is what the TUI's attach prompt stores.

### ATT-04 Open a draft's own attachment

- Classification: GUI parity
- Source anchor: `src/selector.rs`
- Daemon surface: `draft.materialize_attachment`, opened client-side
- GUI location: clients/desktop: `to` on a draft opens its one file or picks among several, and the draft preview's attachment list has an Open button per entry, disabled with a "missing" badge when no file is there; `draft_attachment_open` opens the path `draft.attachments` resolved with the system opener (M3, #0131; reader.md, "Drafts and server-only hits")
- Validation: `tests/cli_selector_contract.rs`; `clients/desktop/src/components/compose/compose.test.tsx` (`to on a draft picks among its files; ta outside Drafts and ts on a draft say why not`, `the preview lists the entries in order, opens one, flags a missing one and removes one`)
- Status: GUI shipped (M3, #0131)
- Note: `draft.materialize_attachment` is not served; a draft's file is already on disk, so the desktop opens the path the frontmatter lists, resolved as the send path resolves it.

### ATT-05 Open a message's HTML part in the browser

- Classification: GUI parity
- Source anchor: TUI `tb` (`clients/tui/src/app/keymap.rs:633`), search overlay `b`
- Daemon surface: `message.materialise_html`, opened client-side through `parse::open_file_with_system`
- GUI location: clients/desktop: `tb`, the reader toolbar's Open in browser and the palette hand the daemon's rendition to the default browser through `html_open`, and a server-only hit's markup through `hit_html_open`, which writes it with the charset and the CSP into the app cache (M3, #0131; reader.md, "The browser rendition")
- Validation: unit tests in `src/parse.rs` for the companion document; `the_browser_gets_the_html_blob_written_to_a_file` and `the_browser_rendition_inlines_cid_images_as_data_uris` (`src/tui_tests/actions_store.rs`); `clients/desktop/src/components/reader/reader.test.tsx` (`tb and the toolbar open the daemon's rendition in the browser, and a message without markup says so`, `opens the hit's own markup in the browser, and one with none has no button`), `clients/desktop/src-tauri/src/attachments.rs` (`html_opens_the_daemons_rendition_and_a_message_without_markup_is_none`, `a_hit_rendition_carries_the_charset_and_the_csp_and_opens`)
- Status: routed (P5-U6); GUI shipped (M3, #0131)
- Note: the daemon writes the rendition rather than the markup: the charset meta, the CSP tag and the `cid:` inlining are three #0037 fixes, and serving unhardened markup through a new door would undo them.
  A message whose sender wrote no markup is `-32602`, which the client renders as its "No HTML version available" line and not as an error.
  The file lands in a handle directory with a lifetime, where the TUI wrote it into the row's own materialisation directory; the handle is deliberately not released, because the browser just launched is holding it (`ATT-01`'s rule).

## Sending, outbox, and the undo hold

### SND-01 Send one approved draft

- Classification: GUI parity
- Source anchor: `mp send <selector> [-y]`, `src/main.rs`, `src/send.rs`
- Daemon surface: `send.draft`
- GUI location: clients/desktop: `x` on an approved draft asks "Send this email?" with "To: <to> - <subject>", and its OK calls `send_draft` with `hold: true`, awaited as the operation kind `send`; the hold card or a notice then says how it ended (M3, #0131; shell.md, "Send")
- Validation: `tests/outbox_integration.rs`, `tests/mime_oracle_integration.rs`, `tests/daemon_send_slice.rs`; `clients/desktop/src/keymap/keymap.test.tsx` (`x on an approved draft asks Send this email?, sends the cursor draft only, and leaves the marks`), `clients/desktop/src/components/mutations/mutation-ui.test.tsx` (`with no hold, the outcome is a notice: Sent in the status region, Failed as an alert`), `clients/desktop/src-tauri/src/commands.rs` (`an_approved_draft_is_sent_without_a_second_approve`), `clients/desktop/src-tauri/src/fixture.rs` (`a_held_send_counts_down_then_sends_files_and_settles`, `send_fail_fails_the_operation_and_parks_a_failed_row`)
- Status: routed (P4-U12); GUI shipped (M3, #0131)
- Note: the preview and the `[y/N]` prompt stay in the client, which renders them from `draft.preview`: a daemon has no stdin, and a run without `-y` prints `Cancelled.` and exits 0 without a single `send.*` call.

### SND-02 Send every approved draft of one account or of all accounts

- Classification: GUI parity
- Source anchor: `mp send-approved [-y] [--all-accounts]` (`src/main.rs`), TUI `cX` (`clients/tui/src/app/keymap.rs:669`)
- Daemon surface: `send.approved`
- GUI location: clients/desktop: `cX` in Drafts and the palette ask "Send all approved emails?" with "In <mailbox label>", and the OK calls `send_approved` with `hold: true` for the selected account, awaited as the operation kind `send_approved`; the end says "Sent N, failed M" or "No approved emails found" (M3, #0131; shell.md, "Send")
- Validation: `tests/outbox_integration.rs`, `tests/daemon_send_slice.rs`; `clients/desktop/src/keymap/keymap.test.tsx` (`cX is Drafts only, and in Drafts sends every approved draft of the account`), `clients/desktop/src/app/reducer.test.ts` (`a batch says Sent N, failed M with its failures as an alert`), `clients/desktop/src-tauri/src/commands.rs` (`send_approved_is_awaited_as_send_approved`), `clients/desktop/src-tauri/src/fixture.rs` (`send_approved_sends_every_approved_draft_behind_one_hold`)
- Status: routed (P5-U6); GUI shipped (M3, #0131) for one account; the `--all-accounts` loop has no GUI path
- Note: `--all-accounts` is a loop in the client over `global_config.accounts` in configuration order, so `send.approved` names one account and a caller that sends `all_accounts` is refused.
  The TUI's `cX` went through it in P5-U6 and derives its line from the settled `{sent, failed}`; an account with neither is the "No approved emails found" the empty scan printed.
  The transport check stays client-side, because its sentence is the key's and the progress line has to say which transport was resolved.

### SND-03 Approve and send the current draft with one key

- Classification: GUI parity
- Source anchor: TUI `x` (`clients/tui/src/app/keymap.rs:573`)
- Daemon surface: `draft.approve` then `send.draft`
- GUI location: clients/desktop: `x` from any pane, or the palette's "Send current draft (approve + send)", on a `draft` status asks "Draft is not approved. Approve and send?", and `send_draft` then validates, approves and sends the cursor draft; a draft being sent refuses `d`, `cA`, `cD`, `e`, `ce` and `x` (M3, #0131; shell.md, "Send")
- Validation: TUI golden frames; `clients/desktop/src/keymap/keymap.test.tsx` (`x on a draft asks the TUI's approve-and-send question, and y sends it with the hold`, `x on received mail says it needs a draft, and n cancels a send`), `clients/desktop/src-tauri/src/commands.rs` (`send_draft_validates_approves_then_sends_with_the_hold`, `a_refused_approve_stops_the_send_with_the_invalid_payload`)
- Status: GUI shipped (M3, #0131)

### SND-04 Undo-send hold with a visible countdown and a cancel key

- Classification: GUI parity
- Source anchor: `email.send_hold_secs` with a 20 second default, resolved daemon-side in `src/daemon/hold.rs`
- Daemon surface: `send.hold_status`, `send.cancel_hold`, `hold: true` on `send.draft` / `send.approved`, and the countdown on `state.event` as `send.hold_started` / `send.hold_tick` / `send.hold_fired` / `send.hold_cancelled`
- GUI location: clients/desktop: each hold is a card in the activity area, "Sending in N s" with the subject, the account, a progress bar and Cancel; `u` cancels the newest held send, as the TUI's does, and the palette has "Cancel the held send" (M2, #0131); `x` and `cX` arm it, and the card of a send this window started says "Sending…" after the fire and then "Sent", "Send cancelled", "Failed: <reason>" or "Partly delivered: …" in an end line mounted empty with the card (M3, #0131; shell.md, "Send")
- Validation: `src/tui_tests/hold.rs`, `tests/daemon_send_hold.rs`, `tests/phase5_undo_send_hold.rs`; contract in [docs/tickets/0125-daemon-hardening.md](tickets/0125-daemon-hardening.md) (P6-U1); `clients/desktop/src/components/mutations/mutation-ui.test.tsx` (the "held sends" block), `clients/desktop/src/app/events.test.tsx` (`counts a hold down from its events and cancels it`), `clients/desktop/src/keymap/keymap.test.tsx` (`u cancels a held send while one counts down, and toggles read otherwise`), `clients/desktop/src-tauri/src/fixture.rs` (`a_simulated_hold_counts_down_and_fires`, `a_cancelled_hold_stops_and_cannot_be_cancelled_twice`, `a_cancelled_send_settles_cancelled_and_leaves_the_draft`), `clients/desktop/src/components/mutations/mutation-ui.test.tsx` (`counts down, says Sending… once fired, then Sent in the status line mounted with the card`, `Cancel on this window's card ends in Send cancelled and frees the draft`), `clients/desktop/src/components/shell/a11y.test.tsx` (`mounts a hold card's end line empty, and the same node takes Sent`)
- Status: routed (P6-U2); GUI shipped (M2, #0131), and armed by the desktop's own sends (M3, #0131)
- Note: the hold is the daemon's, and the TUI keeps only what it renders: the status line, the `u` key and the `App::hold` the events fill.
  `mp send` and `mp send-approved` bypass it by construction (`ANO-7`), because they pass no `hold` and the parameter defaults to off.
  The daemon owns the window: `hold` is a boolean and `email.send_hold_secs` is resolved daemon-side, so a caller that passes nothing bypasses the hold and `send_hold_secs = 0` fires at once with no countdown published.
  When the last client exits mid-hold the daemon cancels the hold and leaves the draft approved, which is what killing the TUI did before the move; a client that merely closed *its* window while another is connected cancels nothing, because a send is durable.
  The desktop renders the holds any client armed, from the bootstrap's `holds` and the four `send.hold_*` events, with the seconds of the last tick, and since M3 it arms its own, always passing `hold: true`.

### SND-05 Send a calendar invitation

- Classification: GUI parity
- Source anchor: `mp send --invite --to --cc --subject --start --end|--duration --location --description --uid --sequence`, `src/main.rs`, `src/calendar.rs`
- Daemon surface: `send.invite`, refused on Microsoft Graph with the reason in the error
- GUI location: clients/desktop: the Calendar view's "New invitation" button and the palette open a form with To, Cc, Subject, Start, End or Duration, Location and Description, which asks for a subject, a start and a recipient before `send_invite` starts `send.invite`, awaited as `send_invite`; on a Graph account the form takes no input and shows the daemon's sentence, read through `invite_refusal` (M4, #0131; shell.md, "New invitation")
- Validation: `tests/imip_integration.rs`, `tests/daemon_send_slice.rs`; `clients/desktop/src/components/calendar/invitation.test.tsx` (`opens from the Calendar view's toolbar in To, sends send_invite and closes; the settle says so`, `asks for a subject, a start and a recipient before any call, and keeps what was typed`, `is disabled on a Graph account with the probe's sentence, from the palette`), `clients/desktop/src/app/invite.test.ts` (`names a partial and an undelivered invitation, a failure and an interruption`), `clients/desktop/src-tauri/src/calendar.rs` (`a_new_invitation_is_checked_for_a_subject_a_start_and_a_recipient_in_that_order`, `send_invite_starts_send_invite_and_passes_a_daemon_refusal_on_as_its_sentence`, `the_graph_probe_answers_the_daemon_sentence_and_starts_nothing`), `clients/desktop/src-tauri/src/fixture.rs` (`send_invite_refuses_in_the_daemon_order_then_files_the_invitation_on_the_agenda`)
- Status: routed (P4-U12); GUI shipped (M4, #0131) as a minimal form with no preview, and the daemon mints the UID, since the form sends none
- Note: start and end accept local time or RFC3339, or a bare date for an all-day event (end the last day, inclusive), duration accepts ISO8601 or the short form, `--uid`/`--sequence` re-send an invitation as an update (#0140), and Graph accounts are refused by `mailypoppins::invite::plan_invite`, which both the client and `send.invite` validate through, so the GUI shows a disabled action with its reason rather than a late failure (`ANO-4`); `send.invite` makes that refusal before it looks at anything else about the invitation.
  The client mints the `UID` while it previews and sends it as a parameter, so the UID a user read is the UID that goes out.

### SND-06 Outbox state visibility

- Classification: GUI parity
- Source anchor: `src/outbox.rs`, surfaced as a TUI badge and status entry
- Daemon surface: `state.bootstrap` outbox summary, then `state.event`
- GUI location: clients/desktop: `go`, the palette's "Open outbox" or the sidebar's outbox line open the account's outbox in the list pane, from `outbox_list`, re-read on every `outbox:<account>` invalidation; each row shows its state chip (Queued, Failed, Sent, copy owed, Partly delivered), its id, time and Message-ID, and the lines `mp outbox list` indents (M3, #0131; shell.md, "Outbox")
- Validation: `tests/outbox_integration.rs`, `tests/daemon_send_slice.rs`; `clients/desktop/src/components/outbox/outbox.test.tsx` (`go opens the selected account's outbox, Escape brings the mailbox back`, `shows each state with its chip, and a partial delivery never as a failure`, `drops every key on the hidden selection, from the list and from the reader`), `clients/desktop/src/app/outbox.test.ts` (`goes stale on an invalidation of its account, and is created by one when never read`), `clients/desktop/src-tauri/src/commands.rs` (`outbox_list_reads_the_listing_and_ever_used`)
- Status: routed (P4-U12) for the `mp outbox list` surface; GUI shipped (M3, #0131); the TUI badge not started
- Note: covers queued, retrying, failed, and partly delivered submissions.

### SND-07 Outbox operator actions

- Classification: daemon administration
- Source anchor: `mp outbox list`, `mp outbox retry <id>`, `mp outbox discard <id>`, `src/main.rs`, `src/outbox.rs`, `tests/outbox_integration.rs`
- Daemon surface: `send.outbox_list`, `send.outbox_retry`, `send.outbox_discard`
- GUI location: not required (daemon administration); the desktop's outbox view offers them anyway: `R` or Retry on a `failed` or `sent_pending_append` row and `d` or Discard on any row, each behind a confirmation that warns about a second delivery or names what the discard gives up (M3, #0131; shell.md, "Retry and discard")
- Validation: `tests/outbox_integration.rs`, `tests/daemon_send_slice.rs`; `clients/desktop/src/components/outbox/outbox.test.tsx` (`R retries the cursor row after the warning, and the settle says how it ended`, `R on a row the daemon will not retry names why and asks nothing`, `d discards the cursor row after the warning, and the sidebar line goes with it`, `a refused discard puts the row back and says why`), `clients/desktop/src/app/outbox.test.ts` (`retries only what the daemon admits, and warns about a second delivery`, `names what a discard gives up, for each state`), `clients/desktop/src-tauri/src/commands.rs` (`outbox_retry_is_awaited_as_outbox_retry_and_a_refusal_awaits_nothing`, `outbox_discard_answers_the_row_and_a_second_discard_is_not_found`)
- Status: routed (P4-U12); GUI shipped (M3, #0131)
- Note: deliberately manual, because a submission that died without a verdict may or may not have been delivered, so the desktop asks first and says so in the confirmation rather than retrying on its own.
  `send.outbox_retry` is an operation and not a command: it re-arms the row and then drains it against SMTP and IMAP.

### SND-08 Partly delivered submission where a recipient was refused

- Classification: GUI parity for the surfacing
- Source anchor: `src/outbox.rs`, `src/send.rs`
- Daemon surface: `state.event` carrying the partly delivered state
- GUI location: clients/desktop: a send some recipients refused ends "Partly delivered: <address (reason)>, …" on its card, which stays until dismissed, and its outbox row reads Partly delivered with the recipients who never got it (M3, #0131; shell.md, "Send" and "Rows")
- Validation: `tests/outbox_integration.rs`, `tests/daemon_send_slice.rs`; `clients/desktop/src/app/reducer.test.ts` (`a partial delivery reads Partly delivered and names the refused recipients, never a plain failure`), `clients/desktop/src/components/outbox/outbox.test.tsx` (`shows each state with its chip, and a partial delivery never as a failure`), `clients/desktop/src-tauri/src/fixture.rs` (`send_partial_refuses_one_recipient_and_keeps_a_partial_row`)
- Status: routed (P4-U12) for the CLI surface; GUI shipped (M3, #0131); the `state.event` half not started, since the desktop reads the state from the send's outcome and from `send.outbox_list` after an `outbox:<account>` invalidation
- Note: only a human can close this state, and the GUI must not present it as a plain failure.

### SND-09 Sent-copy append after submission

- Classification: GUI parity
- Source anchor: `src/imap_client/sent.rs`
- Daemon surface: daemon-internal, reported on `state.event`
- GUI location: clients/desktop: a `sent_pending_append` row shows in the outbox view as "Sent, copy owed" with the mailbox it is owed to, and Retry files the copy after "File the Sent copy of row N?" (M3, #0131; shell.md, "Rows" and "Retry and discard")
- Validation: `tests/outbox_integration.rs`, `tests/daemon_send_slice.rs`; `clients/desktop/src/app/outbox.test.ts` (`says a retry's outcome in each of the row's ends`), `clients/desktop/src-tauri/src/fixture.rs` (`send_pending_append_owes_the_sent_copy`, `a_retried_pending_append_row_files_its_copy`)
- Status: routed (P4-U12) for the CLI surface; GUI shipped (M3, #0131); the `state.event` half not started, since the desktop reads the state from `send.outbox_list`
- Note: implicit workflow driven on the next startup or sync, which is how the outbox drives itself.

## Sync, offline behaviour, and pending operations

### SYN-01 Quick sync and full sync from a client

- Classification: GUI parity
- Source anchor: TUI `ss` (`clients/tui/src/app/keymap.rs:593`) and `sS` (`clients/tui/src/app/keymap.rs:594`)
- Daemon surface: `sync.quick`, `sync.full` as `operation.*` with progress on `state.event`
- GUI location: clients/desktop: `ss` and `sS`, and the palette's "Quick sync" and "Full sync", start a quick or a full sync of the selected account through `sync_trigger`, and a failed or dropped sync stays as an activity alert (M2, #0131)
- Validation: TUI golden frames; the two methods in `tests/daemon_sync_slice.rs`; `a_refused_sync_is_the_sentence_the_daemon_gave` (`src/tui_tests/commands.rs`) and `the_finished_operation_lands_where_the_poll_landed` (`src/tui_tests/events.rs`); `clients/desktop/src/keymap/keymap.test.tsx` (`ss and sS start a quick and a full sync of the selected account`), `clients/desktop/src/components/palette/palette.test.tsx` (`Toggle flag/star and Quick sync run from the palette`), `clients/desktop/src/app/mutations.test.ts` (`starts a sync and awaits its id, and reports one that did not start`), `clients/desktop/src-tauri/src/commands.rs` (`a_sync_is_awaited_as_a_sync`)
- Status: routed (P5-U6); GUI shipped (M2, #0131)
- Note: both are operations rather than commands, and both are durable: a sync a GUI started keeps running, and stays watchable, from the CLI window beside it. `sync.full` takes no `limit`, because a bounded full pass is a quick pass under another name.
  The TUI's two keys went through them in P5-U6, which also collapsed the client-side IMAP/Graph fork: the daemon's pass body loads whichever configuration the account has, so only the progress line still says which transport it is.
  P5-U8 took the wait off the worker thread: a pass is started by `commands::dispatch` and its finish arrives as an `operation.finished` event, so nothing polls `operation.status` any more.

### SYN-02 Sync command options

- Classification: CLI automation
- Source anchor: `mp sync [-n] [--mailbox ...] [--dry-run] [--all-accounts]`, `src/main.rs`, `src/sync/engine.rs`
- Daemon surface: `sync.quick` carrying `limit`, `mailbox` and `dry_run`
- GUI location: not required (CLI automation)
- Validation: `tests/cli_help_snapshot.rs`, `tests/daemon_sync_slice.rs`
- Status: routed (P4-U10); GUI not started
- Note: `--all-accounts` conflicts with `-A` by construction so a cron line cannot silently sync accounts it never named, and that conflict is a contract to preserve (`ANO-9`). `--all-accounts` itself is not on the wire: the client issues one operation per account in configuration order, because the per-account header, the failure denominator and the exit code are all rendering of a per-account result. `mp sync` always calls `sync.quick`, since `-n` has a default and the command has no unbounded form.

### SYN-03 Watch a mailbox through IMAP IDLE

- Classification: daemon administration
- Source anchor: `mp watch [--mailbox] [--timeout N]` with exit code 2 on timeout, `src/main.rs`, `src/imap_client/watch.rs`, `imap_watch` (`clients/tui/src/helpers.rs:45`)
- Daemon surface: `sync.watch` as a client-scoped operation over the daemon's watcher
- GUI location: not required (daemon administration)
- Validation: manual, requires a live server; validation and narrowing in `tests/daemon_sync_slice.rs`; `tests/tui_daemon_recovery.rs` for the runtime's own watch
- Status: routed (P5-U8); GUI not started
- Note: P5-U8 moved `imap_watch` and the Graph poller out of `clients/tui/src/` and into the account runtime (`src/daemon/runtime/watcher.rs`), so the watch runs once per account beside the engine rather than once per client: a round that sees the mailbox move runs a quick tick and publishes `sync.completed`, and no client holds a server connection of its own any more. `sync.watch` is unchanged and is still the one-shot a `mp watch` asks for.
  The on-demand IDLE connection was **not** built and the narrowing of `mp watch --mailbox` to INBOX is recorded in `BACKLOG.md` (P4-U10 took the route the plan recommends). Both sides carry it: the client warns on stderr and rewrites the mailbox before it calls, and the daemon refuses anything but INBOX with `-32602`. `--timeout N` stays client-side (wait, `operation.cancel`, `ℹ Timed out.`, exit 2), because a daemon-side timer would be a second place that knows about one client's patience.

### SYN-04 Startup refresh, asynchronous store open, and background mailbox load

- Classification: GUI parity
- Source anchor: the `Fetch`, `FetchAccount`, and `LoadMailbox` actions in `clients/tui/src/app/types.rs` (`LoadMailbox` at `clients/tui/src/app/types.rs:1611`)
- Daemon surface: `state.bootstrap` returning zeroed counts for an `opening` account, filled by `state.event`
- GUI location: clients/desktop (M1, #0131 read slice)
- Validation: TUI golden frames; `src/tui_tests/golden_frames_daemon.rs`
- Status: routed (P5-U8); GUI not started
- Note: implicit workflow with no command, and the reason a client shows content before sync completes.
  `LoadMailbox` became `message.list` / `draft.list` in P5-U4 and the two fetch arms became `sync.quick` in P5-U6, each still on the worker thread it always had.
  P5-U8 took the wait off those threads: a pass is started by `commands::dispatch` and its finish arrives as an `operation.finished` event, so nothing polls `operation.status` any more.
  The `opening` -> ready transition is still `BgResult::AccountOpened` as well as the bootstrap's, because the store-backed open is what a client with a wedged session has left; the account runtime's readiness now reaches the client as an event beside it.

### SYN-05 Sync health and error surfacing per account

- Classification: GUI parity
- Source anchor: `src/sync_health.rs`
- Daemon surface: `state.bootstrap` health summary, then `state.event`
- GUI location: clients/desktop: a health badge beside each account in the sidebar, which each `sync.completed` updates (M1, #0129)
- Validation: unit tests in `src/sync_health.rs`; `clients/desktop/src/app/reducer.test.ts` (`tracks sync health and account state from events`)
- Status: GUI shipped (M1, #0129)

### SYN-06 Pending operation queue replayed against the server

- Classification: GUI parity
- Source anchor: `src/pending_ops.rs`, `src/ops.rs`
- Daemon surface: `state.event` for queue depth and outcomes; the drains of a `sync.*` pass as `operation.progress`
- GUI location: clients/desktop: the activity area reports each batch the daemon took or refused, a `mutations.rolled_back` as an alert in the TUI's words, and a failed or dropped sync; `ss` and `sS` start a quick or a full sync of the selected account (M2, #0131); the status region shows the queue depth while it is not 0, "3 waiting for the server: 1 in the outbox, 1 sending, 1 change" (M3, #0131; shell.md, "Counts and the queue depth")
- Validation: unit tests in `src/pending_ops.rs`; `clients/desktop/src/app/events.test.tsx` (`brings rows back on a rollback and says so`), `clients/desktop/src/components/mutations/mutation-ui.test.tsx` (the "activity notices" block), `clients/desktop/src/keymap/keymap.test.tsx` (`ss and sS start a quick and a full sync of the selected account`), `clients/desktop/src-tauri/src/fixture.rs` (`a_burst_of_mutations_is_one_drain`, `a_rollback_restores_the_rows_and_says_how_many`), `clients/desktop/src/app/outbox.test.ts` (`counts the queue depth from the outbox, the sends and the pending changes`, `counts a retry of a row the listing already counts as open once, and a failed row's retry as sending`)
- Status: routed (P4-U10) for the sync tick's drains; GUI shipped (M2, #0131) for the outcomes and (M3, #0131) for the queue depth, which counts the queued outbox rows, this window's running sends and retries and its unanswered changes; the daemon publishes no depth of its own pending-operation queue, so a change it took and has not yet replayed is not counted

### SYN-07 Store ingest, reconciliation, and drop-and-rebuild on an unreadable SQLite file

- Classification: diagnostics and maintenance
- Source anchor: `src/ingest.rs`, `src/reconcile.rs`, `src/store/rebuild.rs`, `tests/store_ingest_integration.rs`
- Daemon surface: daemon-internal, reported through `diagnostic.*`
- GUI location: not required (diagnostics and maintenance)
- Validation: `tests/store_ingest_integration.rs`
- Status: not started
- Note: the durable outbox survives a rebuild, which is the invariant this capability must not break.

### SYN-08 Retention garbage collection over cached blobs

- Classification: diagnostics and maintenance
- Source anchor: `mp store gc [--dry-run] [--force] [--all-accounts]`, `src/main.rs`, `src/store/sweep.rs`
- Daemon surface: `diagnostic.store_gc`, for `mp store gc` and for the automatic sweep after every sync alike
- GUI location: not required (diagnostics and maintenance)
- Validation: unit tests in `src/store/sweep.rs`, `tests/daemon_admin_slice.rs` (`mp_store_gc_matches_the_oracle`)
- Status: routed (P4-U14 for `mp store gc`, P4-U15 for the post-sync sweep)
- Note: two safety rules a daemon or GUI port reproduces rather than relaxing (`ANO-5`): the first over-cap run warns and records a marker while the second evicts, and a plan reclaiming more than half the store's blob bytes is refused without `--force`.
  The sweep skips blobs backing a materialized handle a client still holds (`ANO-6`).

### SYN-09 Per-account engine lock

- Classification: daemon administration
- Source anchor: `src/engine_lock.rs`, acquired by `drain_account` (`src/pending_ops.rs:567`) for the mutation queue, by `drain_guarded_at` (`src/outbox.rs:1422`) for the outbox and by `run_sync_guarded_at` (`src/sync/engine.rs`) for the IMAP sync ingest
- Daemon surface: daemon-internal; the account runtime holds it for its lifetime
- GUI location: not required (daemon administration)
- Validation: `tests/outbox_integration.rs`, `tests/engine_lock_ingest.rs`, unit tests in `src/engine_lock.rs`
- Status: not started
- Note: the outbox acquisition is #0116, where a refused drain reports nothing done and opens no session and the holder re-sweeps against a re-read clock.
  Phase 3b (#0122) extended it to the ingest half: `sync::engine::run_sync_guarded` is what `mp sync` and the TUI tick call, a non-holder returns `Ok(None)` before the transport is touched, and both callers report the refusal as information and succeed.
  `run_and_settle` (`src/pending_ops.rs:614`), `run_sync` itself and the Graph loop still take no lock, so `ANO-13` is closed for the IMAP ingest only.

### SYN-10 Microsoft Graph backend for sync and send

- Classification: GUI parity as a backend variant
- Source anchor: `src/graph.rs`
- Daemon surface: the same `sync.*` and `send.*` families over the Graph backend
- GUI location: clients/desktop (M1 onward, #0131), the same surfaces over a Graph account
- Validation: unit tests in `src/graph.rs`
- Status: not started
- Note: delta cursors replace IMAP UID state, and invitation send is unavailable on this backend as recorded in `SND-05`.

### SYN-11 Offline operation

- Classification: GUI parity
- Source anchor: the store read paths and the draft editing paths, which do not require a server
- Daemon surface: `state.event` carrying connectivity; the GUI reaches this state through events rather than by falling back to direct store access
- GUI location: clients/desktop (M1, #0131 read slice)
- Validation: `tests/cli_read_surface_integration.rs`, which runs offline
- Status: not started

### SYN-12 Per-mailbox body-fetch deadline

- Classification: GUI parity for its surfacing
- Source anchor: `[imap] body_fetch_deadline_secs` (`src/config.rs:240`), clamped to 600 at load (`src/config.rs:958`), documented at `website/src/pages/config.astro`; `BODY_CHUNK_SIZE` (`src/imap_client/fetch.rs:525`); `bodies_complete` (`src/sync/mod.rs:167`); the TUI message at `clients/tui/src/helpers.rs:411`
- Daemon surface: `state.event` progress carrying the deadline stop
- GUI location: clients/desktop (M1, #0131 read slice)
- Validation: unit tests in `src/imap_client/fetch.rs`, `clients/tui/src/helpers.rs`
- Status: routed (P4-U10) for `mp sync`, which passes no deadline; GUI not started
- Note: default 30, `0` unbounded; bodies go out newest-first in chunks of 20 with the deadline checked between chunks and never inside a command, and the first chunk always goes out so an expired deadline still makes progress.
  A deadline stop returns `bodies_complete = false`, which defers the prune and the modseq like any other short pass and resumes on the next tick, and the client reports it as progress rather than failure (#0113).

### SYN-13 Tail drain of the outbox and the mutation queue after the sync body

- Classification: GUI parity for its surfacing
- Source anchor: `run_tick_with_drains` (`src/sync/tick.rs:25`), driven by both TUI tick paths and by `mp sync` (`src/main.rs:1405`); the non-fatal head-drain error at `src/main.rs:1348`
- Daemon surface: one `operation.progress` per phase, its `phase` naming which of the five slots reported
- GUI location: clients/desktop (M1, #0131 read slice)
- Validation: unit tests in `src/sync/tick.rs`; the wordings and the label in `tests/daemon_sync_slice.rs`
- Status: routed (P4-U10); GUI not started
- Note: tail report lines carry an " (after sync)" label so they cannot be read as the head's, and a head-drain error prints a warning and continues instead of aborting the sync; the daemon owns the tick after the cutover, so this ordering and this non-fatal error are contracts (#0114). The label is the client's and is derived from the phase name alone (`Phase::as_str`), so the daemon publishes facts and `mp_client::format` decides the words.

### SYN-14 Per-mailbox non-convergence detector

- Classification: GUI parity for its surfacing
- Source anchor: `NONCONVERGING_PREFIX` (`src/sync/engine.rs:34`) over a `nonconverging:{role}` row in the store's meta table; the `mp sync` line at `src/main.rs:1506`; `NON_CONVERGING_MARKER` (`clients/tui/src/helpers.rs:326`) and the status downgrade in `clients/tui/src/bg.rs:12`
- Daemon surface: `state.event` warning carrying the marker
- GUI location: clients/desktop (M1, #0131 read slice)
- Validation: unit tests in `src/sync/engine.rs`
- Status: not started
- Note: the meta row holds `hash:count:streak` for the pass's UID set and warns at streak 2, at 3, and every tenth thereafter, on a still-zero exit code.
  A truncated, incomplete, reset, dry-run, or given-up pass neither counts nor resets, and the Graph loop has no detector, so a client must not present its absence there as convergence (#0115).

### SYN-15 Outbox drain exclusivity under the engine lock

- Classification: daemon administration
- Source anchor: `drain_guarded` (`src/outbox.rs:1401`) and `drain_guarded_at` (`src/outbox.rs:1414`), `send::drain_account` (`src/send.rs:2197`), `tests/outbox_integration.rs`
- Daemon surface: daemon-internal
- GUI location: not required (daemon administration)
- Validation: `tests/outbox_integration.rs`
- Status: not started
- Note: a drain refused the lock does nothing, opens no session, and is a success rather than an error, and the holder re-sweeps up to four times against a clock re-read per sweep (#0116).

### SYN-16 UIDVALIDITY-reset unbind of unverified rows

- Classification: diagnostics and maintenance
- Source anchor: `unbind_rows_on_uids` (`src/ingest.rs:822`), run after the ingest loop of a pass that reported a reset
- Daemon surface: daemon-internal, reported in the `sync.*` result
- GUI location: not required (diagnostics and maintenance)
- Validation: `tests/store_ingest_integration.rs`, unit tests in `src/ingest.rs`
- Status: not started
- Note: every row still parked on a listed UID the pass did not itself ingest moves to the `-id` sentinel, which frees the UID for the message that now wears it and leaves the row rebindable; rows on UIDs the server does not list are left alone, and recovery completes on the next full sync because the download window is positional and the repaired rows sit below it (#0117).

### SYN-17 Mail hooks: run a command when a matching message arrives

- Classification: CLI automation
- Source anchor: none, new after the migration (#0135); `[[accounts.hooks]]` in `config.toml`, `src/daemon/hooks/`, `src/daemon/runtime/hook_runner.rs`, entry points `mp hooks list`, `mp hooks test` and `mp hooks replay`
- Daemon surface: the account runtime's hook runner, daemon-internal, and `hook.list`, `hook.test` and `hook.replay`
- GUI location: not required (CLI automation)
- Validation: unit tests in `src/daemon/hooks/` (the sender check against spoofed, missing and foreign `Authentication-Results`, the cursor, the run), `tests/daemon_hooks.rs`
- Status: shipped (#0135)
- Note: the sender is authenticated from the receiving server's own topmost `Authentication-Results` with exact domain equality, the cursor lives outside the store in `<account_dir>/hooks-state.json`, and a hook never fires for mail older than itself.

## Contacts

### CON-01 Fuzzy contact search over name and address

- Classification: GUI parity
- Source anchor: `mp contacts search [query] [-n] [--account]` (`src/main.rs`), the TUI contacts view `/`, `src/contacts/matcher.rs`
- Daemon surface: `contact.search`
- GUI location: clients/desktop: `Space c`, the sidebar's Contacts entry and the palette show the Contacts view, the selection account's ranked contacts from `contact_search` in a listbox named "Contacts"; `/` focuses its search field, which asks 150 ms after the typing pauses and drops the answer to a query typed over (M4, #0131; shell.md, "Contacts")
- Validation: unit tests in `src/contacts/matcher.rs`, `tests/daemon_admin_slice.rs`; `clients/desktop/src/components/contacts/contacts.test.tsx` (`lists the selected account's ranked contacts in a listbox named Contacts, with their counts and scores`, `/ focuses the search field, typing asks once per pause, and Escape leaves it with the query kept`), `clients/desktop/src/app/contacts.test.ts` (`drops the answer of a query typed over, so the list is asked again for the new one`, `reads an account's list again when the view comes back to it with another query`), `clients/desktop/src-tauri/src/contacts.rs` (`rows_decode_with_the_recipient_quoted_where_the_name_needs_it`), `clients/desktop/src-tauri/src/fixture.rs` (`contact_search_matches_address_and_name_in_score_order_up_to_the_limit`)
- Status: routed (P4-U14); GUI shipped (M4, #0131) with at most 1000 rows per search, where the TUI lists its whole index

### CON-02 Tab-delimited `email` and `name` output for mutt, aerc, and vim

- Classification: CLI automation
- Source anchor: `mp contacts search --parsable`, `src/main.rs`
- Daemon surface: `contact.search` with the parsable projection
- GUI location: not required (CLI automation)
- Validation: `tests/cli_help_snapshot.rs`, `tests/daemon_admin_slice.rs` (`mp_contacts_search_parsable_is_tab_delimited`)
- Status: routed (P4-U14)
- Note: a stable shape other tools already consume, so the daemon migration must not reformat it (`ANO-8`).
  `contact.search` answers rows named `{address, display_name, sent_to, sent_cc, received, score}`, so the tab-delimited line is a projection of the wire row rather than a translation of it.

### CON-03 Rebuild or refresh the contact index from the local message store

- Classification: GUI parity
- Source anchor: `mp contacts rebuild [--account]` (`src/main.rs`), the TUI contacts view `r`
- Daemon surface: `contact.rebuild` as an `operation.*`
- GUI location: clients/desktop: `r`, the Contacts view's "Rebuild index" and the palette's "Refresh contact index" call `contact_rebuild` for the view's account, awaited as `contact_rebuild`; the header says a rebuild runs, and the settle says the TUI's four notices (M4, #0131; shell.md, "Rebuild")
- Validation: unit tests in `src/contacts/`, `tests/daemon_admin_slice.rs`; `clients/desktop/src/components/contacts/contacts.test.tsx` (`r rebuilds the index with the pending state in the header, then says the TUI's four outcomes`), `clients/desktop/src/app/contacts.test.ts` (`says how many a written index holds, and the cache guard's two refusals with what it kept`, `a failed, a dropped and a refused start each say the refresh failed`), `clients/desktop/src-tauri/src/contacts.rs` (`a_rebuild_is_awaited_as_contact_rebuild`), `clients/desktop/src-tauri/src/session.rs` (`a_contact_rebuild_passes_its_progress_and_settles_as_contact_rebuild`), `clients/desktop/src-tauri/src/fixture.rs` (`rebuild_refused_settles_the_next_rebuild_refused_shrunk_once`)
- Status: routed (P4-U14); GUI shipped (M4, #0131) for the account the view shows
- Note: the all-accounts default of the CLI form is automation, while the single-account refresh is the user-facing capability.
  The loop is the client's, over the configured accounts in configuration order; the method takes one required `account` and no `all_accounts`.
  The TUI's `r` joined it in P5-U10c-I2 (#0126): an `Action::RefreshContacts` starting the same operation, where it walked the store on the UI thread and rendered its verdict in place.

### CON-04 Contact index statistics

- Classification: diagnostics and maintenance
- Source anchor: `mp contacts stats [--account]`, `src/main.rs`
- Daemon surface: `contact.stats`
- GUI location: not required (diagnostics and maintenance)
- Validation: `tests/cli_help_snapshot.rs`, `tests/daemon_admin_slice.rs`
- Status: routed (P4-U14)

### CON-05 Compose to a contact from the contacts view

- Classification: GUI parity
- Source anchor: TUI `Enter` and `n` in the CONTACTS keymap section
- Daemon surface: `draft.create` seeded from the contact
- GUI location: clients/desktop: Enter, `n`, a double-click and the header's "Compose" in the Contacts view open the new-draft wizard with the contact's recipient in To and the focus in Subject (M4, #0131; shell.md, "Contacts", "Keys and actions")
- Validation: TUI golden frames; `clients/desktop/src/components/contacts/contacts.test.tsx` (`Enter and n open the new-draft wizard with the contact in To, a comma name quoted, the focus in Subject`), `clients/desktop/src/keymap/keymap.test.tsx` (`in Contacts c copies and arms no compose prefix, so c then n composes to the contact, and Mail's cn still opens a blank draft`)
- Status: GUI shipped (M4, #0131)

### CON-06 Send a contact as a vCard

- Classification: GUI parity
- Source anchor: TUI `v` in the contacts view, `src/contacts/vcard.rs`
- Daemon surface: client-side vCard: the Tauri layer builds it with `mp_core::contacts::contact_to_vcard`, writes the `.vcf` under the drafts' `_vcards/` directory, and attaches it to a draft made by `draft.create`; the daemon serves no `contact.vcard`
- GUI location: clients/desktop: `v`, the header's "Send vCard" and the palette's "Send contact as vCard" call `contact_vcard_draft`, which writes a "Contact: <name>" draft to the contact with the `.vcf` attached, then the draft opens in the external editor (M4, #0131; shell.md, "Contacts", "Keys and actions", rust-layer.md, "The contacts")
- Validation: unit tests in `src/contacts/vcard.rs`; `clients/desktop/src/components/contacts/contacts.test.tsx` (`v writes a vCard draft to the contact and opens it in the editor`), `clients/desktop/src-tauri/src/contacts.rs` (`a_vcard_draft_writes_the_vcf_beside_the_drafts_and_attaches_it_once`, `a_vcard_name_is_the_display_name_else_the_local_part`)
- Status: GUI shipped (M4, #0131); a failure after `draft.create` leaves the draft in Drafts without its `.vcf`

### CON-07 Copy a contact's email address

- Classification: GUI parity
- Source anchor: TUI `c` in the CONTACTS keymap section
- Daemon surface: client-side clipboard write
- GUI location: clients/desktop: `c`, the header's "Copy address" and the palette's "Copy email address" copy the contact's address through `copyText` and say "Copied <address>" (M4, #0131; shell.md, "Contacts", "Keys and actions")
- Validation: TUI golden frames; `clients/desktop/src/components/contacts/contacts.test.tsx` (`c copies the address through the clipboard and arms no c family key`)
- Status: GUI shipped (M4, #0131)

### CON-08 Address extraction and frecency ranking from the message store

- Classification: GUI parity
- Source anchor: `src/contacts/extractor.rs`, `src/contacts/rank.rs`, `src/contacts/hooks.rs`
- Daemon surface: daemon-internal, with `state.event` when the index changes
- GUI location: clients/desktop: the Contacts view lists the daemon's ranking in its order, with each row's To, Cc and received counts and a query's match score; no event names the index, so the view reads the list on every open and after a written rebuild (M4, #0131; shell.md, "Contacts", "Staleness")
- Validation: unit tests in `src/contacts/rank.rs`, `src/contacts/extractor.rs`; `clients/desktop/src/components/contacts/contacts.test.tsx` (`lists the selected account's ranked contacts in a listbox named Contacts, with their counts and scores`, `j, k, G and gg move the cursor, and each open reads the list again`)
- Status: routed (P4-U14); GUI shipped (M4, #0131) as the display of the daemon's ranking; the index-changed event is not built
- Note: implicit workflow that keeps the index current as mail arrives.
  Since the admin slice the extraction, the ranking and the cache guard (#0067) all run in the daemon; no client opens the index.

## Calendar and iMIP

### CAL-01 RSVP to a received invitation

- Classification: GUI parity
- Source anchor: `mp invite accept|tentative|decline <selector> [--mailbox]` (`src/main.rs`), TUI `tv` in the message context and `V` in the calendar view, `src/invite.rs`, `tests/imip_integration.rs`
- Daemon surface: `calendar.rsvp` as an `operation.*`
- GUI location: clients/desktop: the reader's invitation card has Accept, Tentative and Decline, disabled with the TUI's sentence for an invitation that cannot be answered; `tv` from the list or the reader, and `V` or the card's RSVP button in the Calendar view, open the RSVP choice (`a`, `t`, `d`, Enter), which calls `calendar_rsvp`, awaited as `rsvp`, and the settle says "Replied <response> to <summary>" (M4, #0131; reader.md, "Invitations", shell.md, "RSVP")
- Validation: `tests/imip_integration.rs`, `tests/daemon_admin_slice.rs` (`mp_invite_refusals_match_the_oracle`, and the successful reply through the daemon's fake transport); `clients/desktop/src/components/reader/invite.test.tsx` (`shows the event under the header with three enabled replies, and Accept sends and settles`, `tv opens the choice: a, t, d pick, j, k and Tab move, Enter sends the choice`, `opens the choice for the cursor row and sends it; the agenda row follows`, `a Graph account's row shows the daemon's sentence, from the probe`), `clients/desktop/src/app/rsvp.test.ts` (`names the reasons in the TUI's order and words: not a REQUEST, cancelled, superseded, the user's own, then Graph`), `clients/desktop/src-tauri/src/fixture.rs` (`calendar_rsvp_refuses_graph_first_then_settles_with_the_reply`, `rsvp_fail_fails_the_next_rsvp_and_parks_a_failed_outbox_row`), `clients/desktop/src-tauri/src/session.rs` (`an_rsvp_settled_by_the_requery_carries_its_kind_and_reply`)
- Status: routed (P5-U6); GUI shipped (M4, #0131)
- Note: whole-series only in v1, the reply travels as iMIP over SMTP, and the target message must carry an `invite.ics` blob.
  The Graph refusal (`ANO-4`) is made before anything about the selector is examined, so a surface that shows the RSVP buttons disabled can say why without naming a resolvable message.
  P5-U6 routed the TUI's `V` through it and gave the method a `row_id` address beside the selector, because an agenda row carries a `messages.id` and nothing else (#0050).
  Three client-side checks stay ahead of the call, all three to keep the sentence the key has always printed: the row carries no invitation, the account is Graph, SMTP is not configured.

### CAL-02 Agenda view with an upcoming and past toggle and a refresh

- Classification: GUI parity
- Source anchor: TUI `t` and `r` in the calendar view, `src/agenda.rs`, `clients/tui/src/ui/calendar.rs`
- Daemon surface: `calendar.events` (the name `calendar.agenda` this row carried until P5-U10; the served method is `calendar.events`)
- GUI location: clients/desktop: `Space a`, the sidebar's Calendar entry and the palette show the Calendar view, the selection account's agenda from `calendar_events` in a listbox named "Agenda" beside the event card; `t` or "Past events" shows the past events by the TUI's rule, client-side, and `r` or "Refresh" reads the agenda again (M4, #0131; shell.md, "Calendar")
- Validation: unit tests in `src/agenda.rs`, TUI golden frames, `src/tui_tests/invites.rs`; `clients/desktop/src/components/calendar/calendar.test.tsx` (`lists the upcoming agenda of the selected account with its badges, and the card of the cursor row`, `t shows the past events with the TUI's status line and arms no t family key`, `r reads the agenda again and says how many events it shows, without replying`), `clients/desktop/src/app/calendar.test.ts` (`hides past events by default, keeps a running one until its end and an undated one always`, `r that fails with rows shown keeps them and says so in the notice line`), `clients/desktop/src-tauri/src/calendar.rs` (`calendar_events_on_decodes_the_fixture_agenda`), `clients/desktop/src-tauri/src/fixture.rs` (`the_agenda_decodes_sorted_with_the_undated_row_last`)
- Status: routed (P5-U10) - the TUI's agenda is built by the daemon; GUI shipped (M4, #0131)

### CAL-03 Open the source email of an agenda entry

- Classification: GUI parity
- Source anchor: TUI `Enter` and `e` in the calendar view
- Daemon surface: `message.ics`, whose bytes the client writes to a temp file for the editor session
- GUI location: clients/desktop: Enter, `e` or a double-click on an agenda row calls `invite_source_open`, which writes the row's `invite.ics` from `message.ics` into an owner-only file and opens it in the external editor (M4, #0131; shell.md, "Calendar", "Keys and actions")
- Validation: TUI golden frames, `src/tui_tests/invites.rs`; `clients/desktop/src/components/calendar/calendar.test.tsx` (`Enter and e open the cursor row's invite.ics in the editor`, `a row with no ics says so in the TUI's words, and a failed editor names why`), `clients/desktop/src-tauri/src/calendar.rs` (`invite_source_open_writes_a_private_file_and_journals_the_editor`, `a_row_without_an_ics_is_not_found_and_opens_nothing`), `clients/desktop/src-tauri/src/fixture.rs` (`message_ics_answers_the_source_in_base64_or_null`)
- Status: routed (P5-U10c-I2) - what `$EDITOR` gets is the row's `invite.ics` blob and not the message (#0052 scope item 10), so the method is the invitation read rather than a rendition; GUI shipped (M4, #0131)

### CAL-04 Report what stored attendee replies resolve on stored invitations

- Classification: diagnostics and maintenance
- Source anchor: `mp calendar rebuild [--account]`, `src/main.rs`, `src/calendar_cmd.rs`
- Daemon surface: `calendar.rebuild`, which reports and writes nothing
- GUI location: not required (diagnostics and maintenance)
- Validation: `tests/imip_integration.rs`, `tests/daemon_admin_slice.rs` (`mp_calendar_rebuild_matches_the_oracle`)
- Status: routed (P4-U14)
- Note: attendee status is derived from the `invite.ics` payloads wherever it is displayed, so there is no cached copy to rebuild.

### CAL-05 Invitation rendering with derived attendee statuses

- Classification: GUI parity
- Source anchor: `src/invite.rs`
- Daemon surface: `calendar.events` and `message.invite` carry the derived statuses; `message.ics` hands out the raw payload an RSVP is built from
- GUI location: clients/desktop: an agenda row carries one badge in the TUI's order (cancelled, organizer, else the user's reply), and the shared event card shows When, Repeats, Where, Organizer, "Your RSVP", every attendee with their status, the cancelled occurrences and a cancelled or superseded version, in the Calendar view and as the reader's invitation card from `invite_get` (M4, #0131; shell.md, "Rows and the card", reader.md, "Invitations")
- Validation: `tests/imip_integration.rs`, unit tests in `src/invite.rs`, `src/tui_tests/invites.rs`; `clients/desktop/src/app/calendar.test.ts` (`badges cancelled first, then organizer, then the user's reply`), `clients/desktop/src/components/calendar/calendar.test.tsx` (`j, k, G and gg move the cursor and the card follows; the organizer's card has no RSVP and names its cancelled occurrence`), `clients/desktop/src/components/reader/invite.test.tsx` (`shows the event under the header with three enabled replies, and Accept sends and settles`), `clients/desktop/src-tauri/src/calendar.rs` (`invite_get_on_answers_the_card_of_an_invitation_and_null_for_a_plain_email`), `clients/desktop/src-tauri/src/fixture.rs` (`message_invite_answers_the_agenda_event_and_the_version_an_email_carried`)
- Status: routed (P5-U10) - the fold crosses the socket; GUI shipped (M4, #0131)

### CAL-06 Invitation updates and cancellations reflected in the agenda and the reader

- Classification: GUI parity
- Source anchor: implicit workflow driven by newly synced iMIP messages
- Daemon surface: `state.event`
- GUI location: clients/desktop: an account's agenda and invitation cards go stale on a `state.invalidate` or `state.remove` of its mail and on its `sync.completed`, so an update or a cancellation reaches the agenda and an open card without a refresh (M4, #0131; shell.md, "Calendar", "Staleness")
- Validation: `tests/imip_integration.rs`; `clients/desktop/src/components/calendar/calendar.test.tsx` (`follows a cancellation and an update without a manual refresh`), `clients/desktop/src/components/reader/invite.test.tsx` (`follows invite_cancel: the card says cancelled and the replies are disabled with the TUI's sentence`, `follows invite_update: the card shows the new time`), `clients/desktop/src/app/calendar.test.ts` (`goes stale with its own account's mail and syncs only`), `clients/desktop/src-tauri/src/fixture.rs` (`invite_update_moves_the_start_and_raises_the_sequence`, `invite_cancel_flips_the_agenda_row_and_delivers_the_cancellation`)
- Status: GUI shipped (M4, #0131) against the fixture's `invite_update` and `invite_cancel`, which change the agenda row in place where the daemon folds the new email in

## Client-side integrations

### INT-01 Open `config.toml` in the editor

- Classification: GUI parity
- Source anchor: TUI `sc` (`clients/tui/src/app/keymap.rs:596`)
- Daemon surface: `config.get`'s `path` for the location; the daemon reloads the file either way
- GUI location: clients/desktop: `sc` from every view, the palette's "Open config.toml in $EDITOR", and "Open config.toml" in the Settings view and on the config.toml banner call `config_open`, which opens `config.get`'s `path` in the external editor; the Settings view's Reload then calls `config_reload` (M4, #0131; shell.md, "config.toml and the daemon log" and "Settings")
- Validation: `clients/desktop/src/components/activity/activity.test.tsx` (`s c and s f open them from Mail and from Contacts, and s l works there too`, `says why when there is no config.toml or no log yet, and logs it as a warning`), `clients/desktop/src/components/settings/settings.test.tsx` (`Open config.toml hands the daemon's file to the editor`, `Reload says what the swap did, logs it once, and reads the configuration again`), `clients/desktop/src-tauri/src/daemon_files.rs` (`config_open_on_journals_the_editor_on_the_daemons_path`, `config_open_on_an_absent_configuration_is_not_found_and_opens_nothing`, `config_open_on_an_invalid_configuration_still_opens_it`)
- Status: GUI shipped (M4, #0131)
- Note: the GUI equivalent is the settings surface plus an explicit reveal or open action.

### INT-02 Open the log file in the editor

- Classification: GUI parity
- Source anchor: TUI `sf` (`clients/tui/src/app/keymap.rs:597`), the `OpenLogFile` action (`clients/tui/src/app/types.rs:1597`)
- Daemon surface: `diagnostic.log_path`, which answers the dated file the daemon is writing (`<data_dir>/logs/mailypoppins-<date>.log`), the same file the TUI's `sf` opens
- GUI location: clients/desktop: `sf` from every view and the palette's "Open log file in $EDITOR" call `log_open`, which opens the file `diagnostic.log_path` names in the external editor (M4, #0131; shell.md, "config.toml and the daemon log")
- Validation: `tests/daemon_diagnostics.rs`; contract in [docs/tickets/0125-daemon-hardening.md](tickets/0125-daemon-hardening.md) (P6-U7); `clients/desktop/src/components/activity/activity.test.tsx` (`s c and s f open them from Mail and from Contacts, and s l works there too`, `the palette's rows run them`), `clients/desktop/src-tauri/src/daemon_files.rs` (`log_open_on_journals_the_editor_on_the_dated_log`, `log_open_on_a_missing_log_is_not_found_and_opens_nothing`)
- Status: routed (P6-U8); GUI shipped (M4, #0131) as the open-in-editor action; the desktop shows the daemon's log only in the editor
- Note: the GUI provides a log view plus an explicit reveal or open-in-editor action; `mp daemon logs` is the same file paged over the socket.

### INT-03 Clipboard writes for selectors, paths, and addresses

- Classification: GUI parity
- Source anchor: the clipboard actions in `clients/tui/src/actions.rs`
- Daemon surface: client-side in every client
- GUI location: clients/desktop: every copy goes through `copyText` in `src/lib/clipboard.ts`, which calls `navigator.clipboard.writeText` from the key or click handler and says "Copied <what>" or "The clipboard refused <what>": `y` and the reader's Copy menu (sender address, `mp://` link, subject) with their palette rows, `c` on a contact, a refused link's Copy and the device-code dialog's "Copy code"; the clipboard-manager plugin is not installed (M4, #0131; shell.md, "Modules", reader.md, "The toolbar")
- Validation: `clients/desktop/src/lib/clipboard.test.ts` (`writes the text and says what it copied`, `calls the clipboard before it returns, while the key press still counts as a user action`, `says the clipboard refused it when the write is rejected`), `clients/desktop/src/components/activity/activity.test.tsx` (`the Copy menu copies the sender's address, the mp:// link and the subject`, `the palette's copy rows act on the open message, and say so without one`), `clients/desktop/src/components/contacts/contacts.test.tsx` (`c copies the address through the clipboard and arms no c family key`)
- Status: GUI shipped (M4, #0131) against the tests' clipboard stand-in; whether WKWebView takes the write from a key press has not been checked in a real window

### INT-04 Browser launch for HTML parts and OAuth verification URLs

- Classification: GUI parity
- Source anchor: the browser actions in `clients/tui/src/actions.rs`, `src/config_cmd/oauth2.rs`
- Daemon surface: client-side in every client
- GUI location: clients/desktop: a refused link's "Open in browser" in the reader footer calls `open_external` for http, https and mailto only (M1, #0129); `tb` and the toolbar's Open in browser hand the daemon's HTML rendition to the default browser (M3, #0131; reader.md, "The browser rendition"); the device-code dialog's "Open verification page" opens the provider's https URL through `open_external` (M4, #0131; shell.md, "The device-code dialog")
- Validation: `clients/desktop/src/components/reader/reader.test.tsx` (`shows a refused link in the reader footer and opens it only on the click, once`, `cannot hand a non-web scheme to the opener, and the notice dismisses`, `tb and the toolbar open the daemon's rendition in the browser, and a message without markup says so`), `clients/desktop/src/components/settings/wizard.test.tsx` (`a Microsoft 365 account signs in next: the code from the progress, Copy, the verification page, stored`)
- Status: GUI shipped (M1, #0129) for reader links, (M3, #0131) for HTML parts, and (M4, #0131) for the OAuth verification URL

### INT-05 Desktop notifications for new mail

- Classification: GUI parity
- Source anchor: `src/notify.rs`, using osascript on macOS and notify-send on Linux with sanitized payloads
- Daemon surface: the daemon decides a notification is warranted and emits `state.event`; the client holding the entitlement presents it
- GUI location: clients/desktop (M6, #0132)
- Validation: unit tests in `src/notify.rs`; `a_tick_with_arrivals_notifies_the_user_and_refreshes_the_list` (`src/tui_tests/events.rs`), `a_runtime_tick_reaches_a_subscribed_client_with_its_arrivals` (`tests/tui_daemon_recovery.rs`)
- Status: routed (P5-U8); GUI not started
- Note: the daemon decides *what arrived* and the client decides whether to notify. `sync.completed` carries `new_inbox_mail`, `[{from, subject}]` per ingested inbox message, and the TUI reads `notifications = true` on the way to `crate::notify` exactly where it always did (#0009). A client that dropped the event for that setting would drop the status line and the reload with it, so the opt-in is at the notifier and not at the stream.

### INT-06 Editor suspension and resume around an external `$EDITOR`

- Classification: GUI parity
- Source anchor: `Action::suspends_terminal` (`clients/tui/src/app/types.rs:1716`) and the suspend path in `clients/tui/src/lib.rs`
- Daemon surface: client-side
- GUI location: clients/desktop: nothing is suspended; `editor_open` starts the resolved external editor and never waits for it to exit, and the editing banner names the draft and the editor until Done (M3, #0131; rust-layer.md, "Drafts and the editor", shell.md, "The editing banner")
- Validation: unit tests in `clients/tui/src/app/types.rs`; `clients/desktop/src-tauri/src/editor.rs` (`the_env_and_the_setting_win_over_visual_editor_and_the_probes`, `a_nonzero_exit_inside_the_window_is_a_setup_error`), `clients/desktop/src/components/compose/compose.test.tsx` (`names the draft and the editor; Reopen runs the editor again and Done ends the session`)
- Status: GUI shipped (M3, #0131) as the external-editor handoff for GUI editors, and (M5, #0130) as the embedded session for terminal editors
- Note: the GUI replaces suspension with the embedded PTY session.

## Status, activity, logging, and help

### OBS-01 Activity log overlay with scrolling and a filter

- Classification: GUI parity
- Source anchor: TUI `!` (`clients/tui/src/app/keymap.rs:570`), `sl` (`clients/tui/src/app/keymap.rs:595`), and `/` inside the overlay (ACTIVITY LOG keymap section)
- Daemon surface: `state.event` activity stream; the overlay itself is client-side
- GUI location: clients/desktop: `sl`, the sidebar's Activity entry and the palette's "Activity log overlay" open the activity log dialog, the newest 100 lines this window reported (its notices, `sync.completed`, `config.changed`, `config.invalid` and the end of a hold) with their level, scrolled with `j`/`k`, `d`/`u`, `gg`/`G` and filtered with `/`; `!` hides or shows the activity area's notices and never a live hold card, since the desktop has no bottom log pane (M4, #0131; shell.md, "Activity log")
- Validation: TUI golden frames; `clients/desktop/src/components/activity/activity.test.tsx` (`s l lists what the window reported, oldest first, with each level, and Escape closes it`, `filters on text and level: / goes to the field, Enter back to the lines, Escape clears, then closes`, `opens at the end and scrolls with j/k, d/u, gg and G`, `hides the notices and never the live hold card, and shows them again`), `clients/desktop/src/app/activity.test.ts` (`keeps the newest ACTIVITY_LOG_CAP lines, oldest first, with rising ids`, `logs sync.completed at the daemon's severity, config.changed and config.invalid`)
- Status: GUI shipped (M4, #0131); the log holds only what this window heard since it started

### OBS-02 Command palette over every runnable action

- Classification: GUI parity
- Source anchor: TUI `:` (`clients/tui/src/app/keymap.rs:564`) and `Ctrl+p` (`clients/tui/src/app/keymap.rs:565`), `palette_actions()` (`clients/tui/src/app/keymap.rs:841`)
- Daemon surface: client-side, derived from `KEYMAP` so it cannot drift from the bindings
- GUI location: clients/desktop: `:` or `Ctrl+p` opens the command palette over every row of the generated `keymap.json` and the desktop's own rows, each runnable or disabled with its badge (M1, #0129)
- Validation: unit tests in `clients/tui/src/app/keymap.rs`, TUI golden frames; `clients/desktop/src/components/palette/palette.test.tsx` (`lists every action of the generated keymap`, `marks what this build cannot run as disabled with its milestone`, `runs an enabled action`), `clients/desktop/src/keymap/keymap.test.tsx` (`: and Ctrl+p open the palette, ? the key help`)
- Status: GUI shipped (M1, #0129)

### OBS-03 Help overlay and hint bar

- Classification: GUI parity
- Source anchor: TUI `?` (`clients/tui/src/app/keymap.rs:561`), `help_sections()` (`clients/tui/src/app/keymap.rs:787`)
- Daemon surface: client-side, generated from the same `KEYMAP`
- GUI location: clients/desktop: `?` opens the key help, the generated `keymap.json`'s sections followed by the desktop's own keys (M1, #0129)
- Validation: unit tests in `clients/tui/src/app/keymap.rs`, TUI golden frames; `clients/desktop/src/keymap/keymap.test.tsx` (`: and Ctrl+p open the palette, ? the key help`), `clients/desktop/src/components/palette/palette.test.tsx` (`the key help lists the desktop client's own keys after the TUI's`)
- Status: GUI shipped (M1, #0129) for the help overlay; the desktop has no hint bar

### OBS-04 Status line carrying counts, badges, operation progress, and persistent errors

- Classification: GUI parity
- Source anchor: `clients/tui/src/ui/status.rs`
- Daemon surface: `state.bootstrap` summaries, then `state.event` and `operation.*` progress
- GUI location: clients/desktop (M1, #0129)
- Validation: unit tests in `clients/tui/src/ui/status.rs`, TUI golden frames
- Status: not started

### OBS-05 Structured logging into the platform log directory

- Classification: diagnostics and maintenance
- Source anchor: `src/timing.rs`, the `OpenLogFile` action (`clients/tui/src/app/types.rs:1597`)
- Daemon surface: `diagnostic.log_path` and `diagnostic.logs`; the daemon writes its own log, the dated `<data_dir>/logs/mailypoppins-<date>.log` that `src/config.rs` installs and `src/timing.rs` writes its `[TIMING]` lines into
- GUI location: not required (diagnostics and maintenance)
- Validation: unit tests in `src/timing.rs`; the wire surface in `tests/daemon_diagnostics.rs`, contract in [docs/tickets/0125-daemon-hardening.md](tickets/0125-daemon-hardening.md) (P6-U7)
- Status: routed (P6-U8)
- Note: `<data_dir>/logs/daemon.log` is a different file, the stdio of a detached `mp daemon start`, and is empty for a daemon started in the foreground.

### OBS-06 Dump the key bindings as Markdown or JSON

- Classification: CLI automation
- Source anchor: `mp dump-keys [--json]`, `clients/tui/src/app/keymap.rs`, `scripts/regen-website-keys.sh` feeding `website/src/data/tui-keys.json`
- Daemon surface: none; it needs no daemon
- GUI location: not required (CLI automation)
- Validation: `docs/baselines/pre-daemon/tui-keys.json`, byte-identical to the website copy
- Status: not started
- Note: the data feed the GUI key help reuses rather than duplicating (`ANO-8`), and the artifact that goes stale against a newer `keymap.rs` unless it is regenerated after `cargo install --path .` (`ANO-1`).

### OBS-07 Theme configuration

- Classification: GUI parity
- Source anchor: the top-level `theme` key in `config.toml` (`src/config.rs:25`), `clients/tui/src/theme.rs`, `docs/tickets/0023-enable-theme-config.md`
- Daemon surface: client-side; each client reads its own theme at startup
- GUI location: clients/desktop: Settings' Theme buttons (Dark, Light, System) and the palette's "Theme: dark", "Theme: light" and "Theme: system", stored as the `theme` key of `desktop.json` through `setting_get|set`, dark by default, with system following `prefers-color-scheme`; the light palette is `:root.light` in `src/index.css` (#0136; shell.md, "Theme"; design-tokens.md)
- Validation: `test_parse_config_with_theme` (`src/config.rs:1602`); `clients/desktop/src/app/theme.test.tsx` (`puts the light or dark class, color-scheme and its meta on the document`, `system paints the system's scheme and follows its changes until another theme is applied`, `the stored theme is painted at startup and kept in the store`, `Settings' Theme buttons paint at once and store the choice`, `the palette has Theme: dark, light and system, with no key`), `clients/desktop/src/design/contrast.test.ts` (`keeps every pair at or above its minimum`, per palette)
- Status: GUI shipped (after M4, #0136)
- Note: the TUI reads config.toml's `theme` once at startup, so a change needs a restart; the desktop does not read that key, and a theme chosen in its Settings paints at once.

### OBS-08 Quit the client while leaving durable work in place

- Classification: GUI parity
- Source anchor: TUI `q` (`clients/tui/src/app/keymap.rs:557`)
- Daemon surface: client disconnect; the daemon keeps running
- GUI location: clients/desktop (M1, #0129)
- Validation: TUI golden frames
- Status: not started
- Note: in the GUI, closing the window exits the client and leaves the daemon running.

## Daemon administration and lifecycle

Every capability in this group is new in this plan and had no pre-daemon source anchor, so this layer is greenfield and carries no legacy compatibility burden (`ANO-10`).
The source anchors below are the entry points the plan created; the daemon, the socket and the lifecycle commands are all in the tree now.

### LIF-01 Run the daemon in the foreground

- Classification: daemon administration
- Source anchor: none, new in this plan; entry point `mp daemon run`
- Daemon surface: the process itself, binding `<data_dir>/runtime/daemon.sock`
- GUI location: not required (daemon administration)
- Validation: `tests/daemon_lifecycle.rs`, `tests/daemon_runtime_paths.rs`
- Status: shipped (P2-U7, ticket #0120)

### LIF-02 Start a detached daemon and wait for readiness

- Classification: daemon administration
- Source anchor: none, new in this plan; entry point `mp daemon start`
- Daemon surface: `daemon.start.lock` for startup exclusion, then a handshake on the socket
- GUI location: not required (daemon administration)
- Validation: `tests/daemon_lifecycle.rs`, `tests/daemon_runtime_paths.rs`
- Status: shipped (P2-U7, ticket #0120)

### LIF-03 Report daemon status, version, instance, and account health

- Classification: daemon administration
- Source anchor: none, new in this plan; entry point `mp daemon status`
- Daemon surface: `daemon.status` over `daemon.json`
- GUI location: not required (daemon administration)
- Validation: `tests/daemon_lifecycle.rs` (the `--json` key set exactly)
- Status: shipped (P2-U7, ticket #0120)

### LIF-04 Stop the daemon gracefully

- Classification: daemon administration
- Source anchor: none, new in this plan; entry point `mp daemon stop`
- Daemon surface: `daemon.stop {grace_secs?}`, answering with what is still in flight and reporting through a `daemon.stopped` notification what did not settle
- GUI location: not required (daemon administration)
- Validation: `tests/daemon_shutdown.rs` (12 rows over a real socket), `tests/daemon_lifecycle.rs`
- Status: shipped (P2-U7, ticket #0120); made graceful by P6-U4, ticket #0125
- Note: the eight shutdown steps and the `--grace-secs` flag are P6-U4's; `mp daemon stop` exits 0 whether or not everything settled.

### LIF-05 Restart the daemon explicitly

- Classification: daemon administration
- Source anchor: none, new in this plan; entry point `mp daemon restart`
- Daemon surface: `daemon.stop` then a fresh start; exit code 3 names this command on an incompatible daemon
- GUI location: not required (daemon administration)
- Validation: `tests/daemon_lifecycle.rs` (`restart_yields_a_new_instance_id`)
- Status: shipped (P2-U7, ticket #0120)
- Note: also invoked by the GUI mismatch screen after user confirmation.

### LIF-06 Install or remove the login-start service

- Classification: daemon administration
- Source anchor: none, new in this plan; entry points `mp daemon install-service` and `mp daemon uninstall-service`
- Daemon surface: none, and deliberately so: the commands write a systemd user unit or a launchd user agent themselves, and the file they write is what starts a daemon, so there is nobody to ask
- GUI location: not required (daemon administration)
- Validation: `tests/daemon_service.rs` over `tests/fixtures/service/`; contract in [docs/tickets/0125-daemon-hardening.md](tickets/0125-daemon-hardening.md) (P6-U5)
- Status: routed (P6-U6)
- Note: the launchd half cannot be smoke-tested on the machine this project is developed on; the plist is pinned as a fixture and the live check is owner action on macOS, still outstanding.

### LIF-07 Health and support diagnostics

- Classification: diagnostics and maintenance
- Source anchor: none, new in this plan
- Daemon surface: `diagnostic.health`, `diagnostic.logs`, `diagnostic.log_path` and `diagnostic.support_bundle`, fronted by `mp daemon health`, `mp daemon logs` and `mp daemon support-bundle` under the `daemon` subtree; the non-`ok` checks also fill `state.bootstrap`'s `diagnostics` array and travel as the `diagnostic.check_changed` event
- GUI location: not required (diagnostics and maintenance)
- Validation: `tests/daemon_diagnostics.rs` and `clients/tui/src/diagnostics_tests.rs`; contract in [docs/tickets/0125-daemon-hardening.md](tickets/0125-daemon-hardening.md) (P6-U7)
- Status: routed (P6-U8)
- Note: the support bundle is a directory of five files rather than an archive, because this tree links neither `tar` nor `flate2`, and every secret value the configuration carries is struck from every file in it.

### LIF-08 On-demand automatic start from any normal client

- Classification: daemon administration
- Source anchor: none, new in this plan
- Daemon surface: the client spawns `mp daemon run` and waits for readiness; exit code 4 on failure
- GUI location: not required (daemon administration)
- Validation: `tests/daemon_autostart.rs`
- Status: shipped (P4-U2, ticket #0123)
- Note: excludes the lifecycle commands themselves and the commands that read no domain state (`ACC-04`, `OBS-06`).

## Migration-only surfaces

### MIG-01 Report what remains of the file-era `.md` tree and import its drafts

- Classification: migration-only
- Source anchor: `mp cutover [--account] [--dry-run]`, `src/main.rs`, `src/cutover.rs`
- Daemon surface: `config.cutover` as an `operation.*`
- GUI location: not required (migration-only)
- Validation: unit tests in `src/cutover.rs`, `tests/daemon_admin_slice.rs` (`mp_cutover_matches_the_oracle`)
- Status: routed (P4-U14)
- Note: P4-U14 took the first of the two options this row offered; the client-side one is gone, because after the cutover the daemon owns the data directory and the drafts index the import writes into.
- Note: assigns an `id:` field to any draft lacking one so it becomes addressable by selector, names the dead mailbox directories, prints the command that removes them, and deletes nothing itself, while `--dry-run` writes not even the `id:` field.

### MIG-02 Signature migration from the `[accounts.signatures]` TOML block into signature files

- Classification: migration-only
- Source anchor: `migrate_config_signatures` (`src/signatures.rs:265`), with the default recorded in `state.json`
- Daemon surface: runs in the daemon startup sequence after the cutover
- GUI location: not required (migration-only)
- Validation: unit tests in `src/signatures.rs`
- Status: not started

### MIG-03 Store schema migration at account-runtime start

- Classification: migration-only
- Source anchor: `src/store/schema.rs`
- Daemon surface: runs inside the daemon at account-runtime start, so no client performs it
- GUI location: not required (migration-only)
- Validation: `tests/store_ingest_integration.rs`
- Status: not started

### MIG-04 Legacy config-directory migration

- Classification: migration-only
- Source anchor: `migrate_legacy_config_dir` (`src/config.rs:668`), called from `src/main.rs:1672` on every command
- Daemon surface: runs once in the daemon startup sequence, before the first configuration load
- GUI location: not required (migration-only)
- Validation: unit tests in `src/config.rs`
- Status: not started
- Note: an explicit `MAILYPOPPINS_CONFIG_DIR` (`src/config.rs:606`) suppresses the fallback, which is why it belongs to daemon identity and not only to startup; identity is the pair with `MAILYPOPPINS_DATA_DIR` (`src/config.rs:1099`) and a mismatch is refused at the handshake (`ANO-14`).

## Anchor corrections

Every anchor above was resolved against the tree at `f8af44b`.
Four moved or were imprecise in the plan's inventory and are corrected here.

- `MBX-04`: `clients/tui/src/app/keymap.rs:592` is a section comment; the `ga` binding is at line 591.
- `ACC-11`: the `{{SIGNATURE}}` marker is not in `src/signatures.rs`; the splice and strip logic lives at `src/send.rs:185-268`, with the preview substitution at `clients/tui/src/ui/preview.rs:71`.
  `src/signatures.rs` holds the file and default lookup only.
- `MSG-08`: `queue_mark_open_read` is defined at `clients/tui/src/app/mod.rs:1230`; `clients/tui/src/app/keys.rs` calls it (lines 322 and 336), which is what the inventory's wording describes.
- `OBS-07`: there is no `[theme]` config section; `theme` is a top-level key (`src/config.rs:25`).
  `clients/tui/src/theme.rs` resolves.

`ACC-10` was resolved again against the tree of M4 (#0131): the `cs` binding is at `clients/tui/src/app/keymap.rs:603`, and the signature files are `crates/mp-core/src/signatures.rs`.

`RD-05` has no resolvable anchor by design: `clients/tui/src/images.rs` was deleted when #0109 retired the capability.

`SYN-15` cites `src/outbox.rs:1401` for `drain_guarded`, which is correct; `drain_guarded_at` begins at line 1414, and `SYN-09`'s `src/outbox.rs:1422` points at the lock acquisition inside it.
Both resolve as written.

## Capabilities the inventory does not name

Cross-reading `docs/baselines/pre-daemon/cli-help.txt` (every subcommand of every command) and `docs/baselines/pre-daemon/tui-keys.json` (92 bindings across 10 sections) against the entries above leaves no unnamed CLI surface.
Three key surfaces are only implicitly covered.

- `Esc` in the MESSAGE context, which clears the selection and returns to the list, falls under `MSG-06` without being named there.
  The desktop's Escape clears the marks before the selection (M2).
- Half-page `d/u` in the SERVER SEARCH and ACTIVITY LOG overlays sits outside `LST-02`, which names half-page scrolling for the list and body panes only.
- `Tab` inside the SERVER SEARCH overlay switches focus between the result list and the query field, which is overlay-internal focus rather than the global pane cycling of `MBX-06`.

The overlay-internal keys `mp dump-keys` cannot see are inventoried by P0-U2 in `docs/baselines/pre-daemon/manual-keys.md` (`ANO-2`) rather than here.
