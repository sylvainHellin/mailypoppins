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

The matrix is a list rather than a table because eight columns over 133 rows is unreadable in an editor; the field names are the columns.

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
- Source anchor: `mp config init`, `src/main.rs` (`DaemonWizard`), `src/config_cmd/init.rs`
- Daemon surface: `config.get` before the first prompt; `config.probe` for each connection test and the server's mailbox list, `config.oauth2_login` with the `candidate` block for a Microsoft 365 sign-in, then `config.init` (with `replace` when the user agreed to overwrite) and one `config.set_password` per password; `state.event` once the account exists
- GUI location: clients/desktop: the Settings view's and the palette's "Add account" open the account wizard with the CLI's four presets (IMAP and SMTP, Proton Bridge, Microsoft 365 OAuth2, Microsoft 365 Graph), then identity, servers, a Connection step that tests the candidate through `config_probe` (the SMTP and IMAP logins with the passwords typed there, or a device-code sign-in of the candidate through `config_oauth2_login` with `candidate` and then the SMTP test for OAuth2, a warning when refused as in the CLI, or the Graph test), a mailbox step offering the server's list with the CLI's defaults and a typed name besides, and a review that calls `config_add_account`, or `config_init` from the first-run setup screen a daemon with no config.toml shows, then stores the passwords the test used with `config_set_password` (M4, #0131; shell.md, "Account wizard" and "First run")
- Validation: `tests/daemon_admin_slice.rs` (`mp_config_init_prompts_in_the_client`, `mp_config_add_account_tests_the_login_through_the_daemon_and_writes_nothing_when_it_fails`), `tests/daemon_config.rs` (`config_probe_answers_a_refused_session_and_never_echoes_the_password`, `config_init_replaces_an_existing_config_only_when_asked`, `config_oauth2_login_takes_a_candidate_with_the_configured_refusals`), `src/config_cmd/tests.rs` (the candidate blocks through the loader, `a_server_listing_only_inbox_lets_the_other_roles_be_typed`, `an_oauth2_account_tests_smtp_after_the_sign_in_and_goes_on_when_it_fails`), `crates/mp-core/src/oauth2.rs` (`the_sweep_drops_only_staged_sign_ins_past_their_age`); `clients/desktop/src/components/settings/wizard.test.tsx` (`refuses to move on without a name, with a taken one, and without the SMTP host`, `tests the logins before anything is written, shows the server's refusal, and stores the password once the account is`, `a Microsoft 365 account signs its candidate in on the Connection step, tests it, and the write promotes the sign-in`, `a wizard cancelled after a passed test writes and stores nothing`, `names a probe refused for a configured name as a taken name`, `a server that lists only INBOX still gets every role: the archive and the sent folder are typed`, `a wizard cancelled while it tests the connection sends the password to no further probe`, `shows the setup screen on a daemon with no config.toml, and its wizard tests and writes the first one`), `clients/desktop/src/app/wizard.test.ts` (`every preset's draft has only account_block's keys and no password`, `follows the CLI's presets and fallbacks`, `probes what the CLI probes, in its order, and only a password login stops the wizard`, `picks the CLI's preferred names from the server's list, each taken once, and leaves the marked roles to the server`, `a Microsoft 365 account's refused SMTP test is a warning that names SMTP AUTH, as in the CLI`, `stops before the next probe once the wizard is gone`), `clients/desktop/src-tauri/src/configuration.rs` (`every_presets_draft_serialises_to_account_block_keys_and_nothing_else`, `config_init_after_config_absent_writes_the_first_configuration`, `a_probe_tests_the_candidate_and_lists_its_mailboxes_writing_nothing`, `a_candidate_sign_in_is_staged_for_its_probes_and_promoted_by_the_write`)
- Status: routed whole since the daemon leftovers (2026-10-22): the prompts are the client's and every engine step is a daemon call, so the wizards left `CLI_ENGINE_RESIDUE`; GUI shipped (M4, #0131), with the connection test and the server mailbox pick through `config.probe` since the desktop wizard parity work, so the CLI and the desktop make the same calls
- Note: the CLI and the desktop describe an account with one `[[accounts]]` block, the `AccountDraft` the daemon's `account_block` writes, and store its password with `config.set_password` once the account exists.
  Nothing is written before the last prompt: a password the probe accepted is held in the client until `config.init` or `config.add_account` has answered, so an abandoned wizard leaves `config.toml` and the secrets backend as they were.
  A password is read masked on a terminal and as a line of stdin otherwise, so a script can answer the wizard.

### ACC-02 Add a further account to an existing configuration

- Classification: GUI parity
- Source anchor: `mp config add-account`, `src/config_cmd/init.rs`
- Daemon surface: `config.get` before the first prompt; `config.probe`, `config.oauth2_login` with a `candidate`, `config.add_account`, `config.set_password`, `state.event`
- GUI location: clients/desktop: the account wizard tests the candidate on its Connection step (`config_probe`, and `config_oauth2_login` with `candidate` for Microsoft 365) and its review calls `config_add_account` while config.toml exists, then `config_set_password`; a refusal such as a taken name stays in the wizard's alert, and the daemon's `config.changed` brings the account and its mailboxes into the sidebar (M4, #0131; shell.md, "Account wizard")
- Validation: `tests/daemon_admin_slice.rs` (`mp_config_add_account_refuses_without_a_configuration`, `mp_config_add_account_tests_the_login_through_the_daemon_and_writes_nothing_when_it_fails`); `clients/desktop/src/components/settings/wizard.test.tsx` (`tests the logins before anything is written, shows the server's refusal, and stores the password once the account is`, `shows the daemon's refusal of the write and stays open`), `clients/desktop/src/app/settings.test.ts` (`config.changed reads the updated account's mailboxes and list again, and leaves the others`), `clients/desktop/src-tauri/src/configuration.rs` (`config_add_account_appends_the_block_and_serves_a_ready_account`, `a_draft_with_a_password_or_any_unknown_key_is_refused`)
- Status: routed whole since the daemon leftovers (2026-10-22); GUI shipped (M4, #0131)
- Note: the refusal when there is no configuration to add to is the daemon's answer, rendered here; the wizard past it is `ACC-01`'s note.

### ACC-03 Show the effective configuration

- Classification: diagnostics and maintenance
- Source anchor: `mp config show`, `src/config_cmd/show.rs`
- Daemon surface: `config.get`, `account.credentials`, `signature.list`, `signature.read`
- GUI location: not required (diagnostics and maintenance); the Settings cards show the same password and token facts (`ACC-05`)
- Validation: `tests/daemon_admin_slice.rs` (`mp_config_show_matches_the_oracle`), `tests/daemon_config.rs` (`account_credentials_reports_presence_and_never_a_value`)
- Status: routed (P4-U14) for the configuration, and since the daemon leftovers (2026-10-22) for the password column and the token line
- Note: the output is redacted, and after the cutover the daemon is the only reader of the underlying `config.toml`.
  The `password = **** / (not set)` column and the `token = valid|expired|invalid|not cached` line render `account.credentials`, which answers presence and a token verdict and never a value; `config.get` stays contracted *not* to probe a secret (`tests/daemon_config.rs`), because answering "is a password stored" on every configuration read is what turns a redacted read into a secret read.
  The signature listing renders `signature.list` and `signature.read`.
  One divergence from the `pre-daemon` binary is deliberate and older than this slice: `config.get` reports the effective configuration, which `docs/daemon-protocol.md` defines as the document *after serde defaults*, so `mp config show` prints `smtp.port = 465` and `imap.port = 993` for a configuration that omits them where the oracle printed `0`.
  The `ACC-03` parity row names every port in its fixture, which takes the difference out of the comparison and leaves the row measuring what it is about.

### ACC-04 Print the configuration file path

- Classification: diagnostics and maintenance
- Source anchor: `mp config path`, `src/config_cmd/mod.rs`
- Daemon surface: client-side; the path is computed, so the command must keep running with no daemon
- GUI location: not required (diagnostics and maintenance)
- Validation: `tests/daemon_admin_slice.rs` (`mp_config_path_never_contacts_a_daemon`)
- Status: client-side, confirmed (P4-U14)
- Note: on `needs_daemon`'s no-daemon list for good, and from P4-U14 the `UNMIGRATED` control row of `tests/daemon_parity_harness.rs`: it is the only *whole command* a daemon-era binary answers in process. The startup preamble every command runs is the other in-process code path, a row of `CLI_ENGINE_RESIDUE` in `tests/architecture_boundaries.rs`; the server leg of `mp search` (`LST-06`) left that table when it moved onto `message.search_server` (PERSO-175).

### ACC-05 Store an SMTP or IMAP password in the active secrets backend

- Classification: GUI parity
- Source anchor: `mp config set-password <smtp|imap> [--account]`, `src/main.rs`, `src/config_cmd/password.rs`
- Daemon surface: `config.set_password`
- GUI location: clients/desktop: a password account's card in the Settings view has "Set SMTP password" and "Set IMAP password", which open a masked dialog whose Enter calls `config_set_password`; the value lives in the dialog's own state only and is emptied on every submit and close (M4, #0131; shell.md, "The password dialog")
- Validation: `tests/secrets_integration.rs` for the backend, `tests/daemon_admin_slice.rs` (`mp_config_set_password_reads_the_password_in_the_client`); `clients/desktop/src/components/settings/settings.test.tsx` (`takes a masked, unremembered value in a labelled field, focused on open`, `stores the password, says so without the value, and keeps it nowhere in the model`, `clears the value when it closes, and shows a refusal with the field empty for a retry`), `clients/desktop/src-tauri/src/configuration.rs` (`a_stored_password_is_in_no_journal_no_debug_and_no_tracing_line`, `a_refused_password_names_the_account_and_never_the_value`)
- Status: routed (P4-U14); GUI shipped (M4, #0131); the card says whether each password is stored and an OAuth2 or Graph account's token state, from `account_credentials` (`account.credentials`), read again after a stored password and a sign-in (`settings.test.tsx`, `says whether each password and token is stored, and reads it again once a password is stored`; `configuration.rs`, `account_credentials_say_whether_a_password_is_stored`)
- Note: secret values travel only on the local socket and never appear in logs, diagnostics, or protocol errors.

### ACC-06 OAuth2 device-code login

- Classification: GUI parity
- Source anchor: `mp config oauth2-login [--account]`, `src/config_cmd/oauth2.rs`, `src/oauth2.rs`
- Daemon surface: `config.oauth2_login` as an `operation.*` whose one `operation.progress` event, phase `device_code`, carries the verification URL and the user code; `operation.cancel` settles it `cancelled` and leaves the provider's poll running
- GUI location: clients/desktop: "Sign in" on an OAuth2 or Graph account's card in the Settings view, and the account wizard's two Microsoft 365 presets, call `config_oauth2_login`, awaited as `oauth2_login`; the device-code dialog shows the code from that operation's progress with "Copy code" and "Open verification page", says "OAuth2 token acquired and cached for account '<name>'" once stored, and its "Cancel sign-in" and Escape call `config_oauth2_cancel` (M4, #0131; shell.md, "The device-code dialog")
- Validation: `tests/daemon_admin_slice.rs` pins the three refusals and the rendering; `clients/desktop/src/components/settings/wizard.test.tsx` (`a Microsoft 365 account signs its candidate in on the Connection step, tests it, and the write promotes the sign-in`, `shows the code a re-bootstrap's operation status carries when the progress event was missed`, `Cancel cancels the operation and says the sign-in may still complete`), `clients/desktop/src/app/signin.test.ts` (`reads the code from its own operation's progress and settles stored`, `leaves the wizard's overlay open, and its end leaves no notice`), `clients/desktop/src-tauri/src/session.rs` (`a_device_code_reaches_only_its_awaited_sign_in_which_settles_as_oauth2_login`, `a_denied_sign_in_fails_with_the_providers_sentence`, `a_cancelled_sign_in_stays_awaited_until_its_cancelled_finish`), `clients/desktop/src-tauri/src/configuration.rs` (`a_sign_in_is_refused_with_the_daemons_sentences`); the flow against a live provider is manual
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
- Source anchor: the global `-A/--account`, refused on the commands that act on no account (`refuse_account_where_meaningless`) and opening the TUI on the named account when no subcommand is given (`App::open_on_account`), and `-s/--signature` and `--no-signature` on `mp new`, `mp reply`, `mp forward` and `mp send --invite` (`SignatureFlags`), in `src/main.rs`; `crates/mp-core/src/signatures.rs`
- Daemon surface: an account parameter on every domain method, `signature.list`, `state.bootstrap`
- GUI location: clients/desktop: the sidebar lists every account, the selected one expanded, and the `ga` account picker or a mailbox chosen in the sidebar selects the active account (M1, #0129; #0147); a new draft's signature is chosen in the `cn` wizard's Signature select, filled from `signature_list` with the account's default preselected and "none" last (M3, #0131)
- Validation: `tests/cli_help_snapshot.rs` pins where each flag is offered, and `src/main.rs`'s unit tests (`the_account_selector_is_refused_where_it_names_no_account`, `the_signature_flags_are_only_on_the_draft_writers`) where each is refused; `tests/cli_tui_launch.rs` (`the_tui_refuses_an_account_dash_a_does_not_name_before_it_starts`) and `clients/tui/src/app/keys.rs` (`the_tui_opens_on_the_account_dash_a_names`) for the TUI launch; `clients/desktop/src/app/reducer.test.ts` (`keeps a choice made between a failed list_accounts and its retry`, `stays with the user's account across a later re-bootstrap`), `clients/desktop/src/components/compose/compose.test.tsx` (`has the TUI's fields, the default signature preselected, and a none option`, `none carries no signature`)
- Status: GUI shipped (M1, #0129) for the account selector and (M3, #0131) for a new draft's signature; the forward wizard has one too (`ACC-11`), and a reply carries the account's default with no choice, although `draft.reply` takes `signature` and `no_signature` as the CLI's `mp reply -s` uses them
- Note: the GUI equivalent is the active-account selector plus a per-composition signature choice.

### ACC-10 Signature file management

- Classification: GUI parity
- Source anchor: the TUI `cs` overlay (`clients/tui/src/app/keymap.rs`, `clients/tui/src/app/keys.rs`), `mp signature list|show|new|edit|rename|delete|default` (`routed_signature` in `src/main.rs`), `crates/mp-core/src/signatures.rs` behind the daemon's `signature.*` family (`src/daemon/methods/signature.rs`), the per-account default recorded in `state.json`
- Daemon surface: `signature.list`, `signature.read`, `signature.create`, `signature.rename`, `signature.delete`, `signature.set_default`, and the watcher's `signature.changed` and `signature.removed`; no method writes content, the editor does
- GUI location: clients/desktop: `cs` in Mail and the palette's "Manage signatures" open the Signatures dialog, which lists every signature with the account's default, previews the cursor one, and creates (`n`), renames (`r`), edits in the external editor (`e`), deletes behind a confirmation (`d`) and sets or clears the default (Enter), each one `signature.*` call (M4, #0131; the daemon leftovers, 2026-10-22; shell.md, "Signatures", rust-layer.md, "Signatures")
- Validation: `tests/daemon_draft_watch.rs` (`the_signature_methods_write_and_the_watcher_announces_every_change`, `a_deleted_signature_settles_into_signature_removed`), `tests/cli_signature.rs` (`mp_signature_manages_the_files_and_each_accounts_default_through_the_daemon`, `a_refused_signature_call_is_the_daemons_sentence_and_exit_1`), `src/tui_tests/signatures.rs` (the TUI overlay over an in-process daemon), `clients/tui/src/actions.rs` (`compose_to_contact_takes_its_signatures_from_the_daemon`, `a_signature_is_edited_at_the_path_the_daemon_names`); `clients/desktop/src/components/signatures/signatures.test.tsx` (`cs lists every signature with the account's default marked and previews the selected one`, `Enter makes the selected signature the default, and clears it when it already is`, `n asks for a name, Enter creates the signature and opens it in the editor`, `r renames from a field seeded with the name, and the default follows`, `d asks first with the file's path; n keeps it, y deletes it`, `signature.changed reads the listing and the preview again while it is open`), `clients/desktop/src/app/signatures.test.ts` (`signature.changed, signature.removed, the dialog's own change and a bootstrap make every listing stale`), `clients/desktop/src-tauri/src/signatures.rs` (`create_writes_an_empty_file_and_refuses_a_name_taken`, `rename_carries_the_default_and_moves_the_file`, `deleting_the_default_clears_it_and_publishes_signature_removed`, `set_default_sets_and_clears_the_accounts_default`)
- Status: routed in the TUI, the CLI and the desktop (the daemon leftovers and their TUI follow-up, 2026-10-22), GUI shipped (M4, #0131); another client's change reaches an open dialog or overlay through `signature.changed` and `signature.removed`
- Note: `mp signature` is the CLI's equivalent, one `signature.*` call per subcommand and `$EDITOR` on the path `signature.read` names for `edit`; `mp config show`'s listing reads `signature.list` and `signature.read` too.
  The TUI's compose wizard lists, opens and creates signatures through the same methods since the follow-up, so no client reads the signatures directory itself.
  Every signature is a file under `signatures/`: the startup migration of #0107 turned the inline-text entries of `config.toml` into files, so none is edited through a temporary copy.

### ACC-11 Signature injection into a draft

- Classification: GUI parity
- Source anchor: the quote marker helpers (`QUOTE_MARKER`, legacy `{{SIGNATURE}}`, `is_quote_marker_line`) and the signature splice in `crates/mp-core/src/draft.rs`, their use in `markdown_to_html` and `plain_text_body` in `src/send.rs`, the file and default lookup in `src/signatures.rs`
- Daemon surface: `draft.create`, `draft.reply`, `draft.forward`, and `draft.set_headers` with `splice_signature` all re-splice
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
- Source anchor: `mp list-mailboxes`, `src/main.rs`, `src/imap_client/mod.rs`, `src/catalogue/discover.rs`
- Daemon surface: `mailbox.list_server`, whose rows carry the role and the policy the catalogue resolves (#0141)
- GUI location: not required (diagnostics and maintenance)
- Validation: manual, requires a live server; refusals and routing in `tests/daemon_sync_slice.rs`
- Status: routed (P4-U10); GUI not started
- Note: a live server call, distinct from the mailbox hierarchy the store already holds. The result carries `source` (`imap` or `graph`) because the two transports report different things about a mailbox: Graph's folder list has the item counts this listing prints and IMAP's `LIST` has the attributes and the delimiter instead.

### MBX-02 Browse and select mailboxes in the sidebar

- Classification: GUI parity
- Source anchor: TUI `j/k` and `Enter` (`clients/tui/src/app/keymap.rs:1147-1163`), `gm` (`clients/tui/src/app/keymap.rs:756`), `clients/tui/src/ui/sidebar.rs`
- Daemon surface: `state.bootstrap` mailbox summaries, `message.list_stream` on selection (#0138)
- GUI location: clients/desktop: the sidebar lists the mailboxes of the selected account and of any account expanded by a click on its header, the others collapsed to one header row with the inbox's unread count (#0147); `gm` focuses it, `j`/`k` move its cursor, and Enter or a click opens the mailbox with the cursor on its top row, as the TUI's `switch_mailbox` does (M1, #0129; PERSO-94)
- Validation: TUI golden frames under `clients/tui/src/`; `clients/desktop/src/app/reducer.test.ts` (`puts the cursor on the top row of a mailbox once its list lands`, `walks the sidebar cursor over the expanded accounts only`), `clients/desktop/src/components/sidebar/accountPicker.test.tsx` (`starts with only the selected account expanded, and a header click toggles one without selecting it`), `clients/desktop/src/keymap/keymap.test.tsx` (`the sidebar cursor moves with j and Enter opens the mailbox`), `clients/desktop/src/components/shell/a11y.test.tsx` (`marks the selected mailbox as the current page`)
- Status: GUI shipped (M1, #0129)

### MBX-03 Jump to a mailbox by digit 1 through 9

- Classification: GUI parity
- Source anchor: `clients/tui/src/app/keymap.rs:628`
- Daemon surface: client-side over the bootstrap mailbox list, then `message.list_stream`
- GUI location: clients/desktop: `1` to `9` open the selected account's nth mailbox (M1, #0129)
- Validation: TUI golden frames; `clients/desktop/src/keymap/keymap.test.tsx` (`digits jump to a mailbox of the selected account`)
- Status: GUI shipped (M1, #0129)

### MBX-04 Switch account

- Classification: GUI parity
- Source anchor: TUI `ga`, the KEYMAP row of `KeyAction::SwitchAccount` in `clients/tui/src/app/keymap.rs`, guarded by `Guard::MultiAccount` so it appears only with more than one configured account; it opens the account picker, `Overlay::Account` (#0147)
- Daemon surface: `account.list`, a second `state.bootstrap`
- GUI location: clients/desktop: `ga` and the palette's "Switch account" open the account picker, which lists every account with its inbox's unread count, the runtime state of one not serving (`opening` or `blocked`, in the TUI as well, from the bootstrap and `account.state_changed`) and a warning mark on one whose last sync failed (the TUI's row says `sync failed`); `j`/`k` move, Enter switches and opens the account's inbox, `1` to `9` switch directly, Escape and `q` cancel; a mailbox chosen in the sidebar selects the account it belongs to (M1, #0129; #0147)
- Validation: TUI golden frame `golden_account_picker` with its daemon twin, and the account picker tests in `clients/tui/src/app/keys.rs` and `clients/tui/src/ui/overlays.rs`, `a_blocked_snapshot_marks_the_account_blocked_until_a_ready_one` and `a_failed_sync_health_in_the_snapshot_marks_the_account` (`clients/tui/src/app/bootstrap.rs`), `an_account_state_change_marks_the_runtime_blocked_and_ready_clears_it` and `a_ready_after_an_opening_resync_ends_the_opening_and_recounts` (`src/tui_tests/events.rs`); `clients/desktop/src/app/reducer.test.ts` (`keeps a choice made between a failed list_accounts and its retry`, `stays with the user's account across a later re-bootstrap`, `open_account_picker opens the overlay with two accounts and is a no-op with one`), `clients/desktop/src/components/sidebar/accountPicker.test.tsx`
- Status: GUI shipped (M1, #0129; picker #0147)

### MBX-05 Switch between the Mail, Contacts, and Calendar views

- Classification: GUI parity
- Source anchor: TUI `Space m`, `Space c`, `Space a` (`clients/tui/src/app/keymap.rs:842-862`)
- Daemon surface: client-side; the view switch reads data already bootstrapped
- GUI location: clients/desktop: `Space m`, `Space c` and `Space a`, the sidebar's Contacts, Calendar and Settings entries and the palette's "Switch to Mail view", "Switch to Contacts view", "Switch to Calendar view" and "Open settings" switch between Mail and the full-pane views, which keep the selection, the marks and the outbox view; Settings is a fourth view with no key (M4, #0131; shell.md, "Views")
- Validation: TUI golden frames; `clients/desktop/src/keymap/keymap.test.tsx` (`Space c, Space a and Space m switch the view, and Escape comes back to Mail`, `a view's own key runs before any prefix arms, and the prefix arms again in Mail`), `clients/desktop/src/app/reducer.test.ts` (`switches to a view and back keeping the selection and the marks, with focus on the view's pane`), `clients/desktop/src/components/shell/a11y.test.tsx` (`makes the views sidebar buttons, one of them or a mailbox the current page, and names each view's region`)
- Status: GUI shipped (M4, #0131)

### MBX-06 Cycle pane focus and zoom the focused pane

- Classification: GUI parity
- Source anchor: TUI `Tab` (`clients/tui/src/app/keymap.rs:636`), `Shift+Tab` (`clients/tui/src/app/keymap.rs:644`), `z` (`clients/tui/src/app/keymap.rs:685`), `Enter` in the MESSAGE section (`KeyAction::FocusBody`), which moves the keyboard from the list or the headers into the body pane as the desktop's Enter on a row focuses the reader (PERSO-92), and the `g` pane jumps `gm`, `gl` and `gr` (`KeyAction::GoMailbox`, `GoList`, `GoReader`)
- Daemon surface: client-side
- GUI location: clients/desktop: `Tab` and `Shift+Tab` cycle sidebar, list and reader from the pane the model holds as focused, `gm` focuses the sidebar, `gl` the list, and `gr` or the palette's "Go to reader (body)" the reader, the TUI's three `g` pane jumps, and `z` zooms the focused list or reader; the focused pane carries `data-focused="true"`, drawn as a 1px `ring` line inside its edge, square where it meets the neighbouring pane (M1, #0129; PERSO-95; shell.md, "Focus order")
- Validation: TUI golden frames, `clients/tui/src/app/keys.rs` (`enter_on_a_received_row_focuses_the_body_pane`, `enter_in_the_headers_and_body_panes_stays_on_the_body`, `g_prefix_jumps_to_the_reader_the_list_and_the_sidebar`, `gr_in_a_narrow_terminal_zooms_the_body_pane`); `clients/desktop/src/keymap/keymap.test.tsx` (`Tab and Shift+Tab cycle focus through sidebar, list and reader`, `marks the pane holding the focus with data-focused, and gr or the palette's Go to reader focus the reader`, `gl focuses the message list from the sidebar and the reader, as the TUI's gl`), `clients/desktop/src/components/shell/layout.test.tsx` (`places the panes where index.css squares the focus ring's shared corners`), `clients/desktop/src/app/reducer.test.ts` (`cycles focus through sidebar, list and reader and walks back through history`, `focus_reader focuses the reader from any pane, brings Mail back from a view, and cycles on from there`); zoom: `keymap.test.tsx` (`z zooms the focused list pane, hiding the reader`), `clients/desktop/src/components/reader/readerMode.test.tsx` (`z zooms the reader in text mode, and j scrolls the reader's container`)
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

### MBX-08 Every server folder in the sidebar, and a picker to open one by name

- Classification: GUI parity
- Source anchor: the daemon's mailbox catalogue (`src/catalogue/`, #0141); TUI `gf` (`clients/tui/src/app/keymap.rs`, `open_folder_picker` in `clients/tui/src/app/keys.rs`)
- Daemon surface: `mailbox.list` and the bootstrap's `mailboxes` rows with `server` and `policy`; a re-seeded hierarchy announced by `state.invalidate` and `state.remove` of `mailbox:<account>/<slug>`
- GUI location: clients/desktop: the sidebar lists every folder the daemon does not ignore, with trash and junk icons; `gf` and the palette's "Go to mailbox (fuzzy picker)" open `FolderPicker`, which filters by label or server name and opens the mailbox; the account totals count `sync` mailboxes only (#0141)
- Validation: `src/catalogue/tests.rs`, `src/daemon/methods/mailbox.rs` (`mailbox_list_serves_every_listed_folder_with_its_policy`), `src/daemon/runtime/publish.rs` (`a_new_folder_reseeds_the_account_and_is_announced`), `clients/tui/src/app/keys.rs` (`the_folder_picker_opens_any_mailbox_by_name`), `clients/tui/src/app/bootstrap.rs` (`a_recount_adopts_the_folders_a_tick_discovered`), `clients/desktop/src/components/sidebar/folderPicker.test.tsx`, `clients/desktop/src-tauri/src/commands.rs` (`account_totals_leave_counts_only_folders_out`)
- Status: GUI shipped (#0141)
- Note: the TUI adopts a changed hierarchy from the `mailbox.list` its recount reads, so a folder created on the server reaches the sidebar at the next full tick.

## Listing, filtering, and search

### LST-01 List received messages offline, grouped by mailbox

- Classification: GUI parity
- Source anchor: `mp list-messages [--mailbox] [-n]`, `src/main.rs`, `list_mailbox` (`src/store/read.rs:178`)
- Daemon surface: `message.list`, one call per listed mailbox
- GUI location: clients/desktop: the list pane shows the selected mailbox, the Drafts mailbox as the draft listing, and reads it again on a `state.invalidate` (M1, #0129)
- Validation: `tests/cli_read_surface_integration.rs`, `tests/daemon_read_slice.rs`; `clients/desktop/src/app/events.test.tsx` (`refetches the shown list when its mailbox is invalidated`), `clients/desktop/src-tauri/src/commands.rs` (`the_drafts_mailbox_branches_to_the_draft_listing`), `clients/desktop/src-tauri/src/fixture.rs` (`listing_rows_decode_as_protocol_rows`)
- Status: routed (P4-U4); GUI shipped (M1, #0129)
- Note: the mailbox argument accepts a role, a slug, the sidebar label or the server name, and the default lists every mailbox of the account. The name is resolved against the daemon's `mailbox.list`, so every folder the server lists is reachable (#0141) and an unknown one is refused in the words it has always been refused in.
  The server Drafts folder is never a received mailbox in either client: `mailbox.list` serves only the local Drafts view for it, since the drafts mirror reads the folder and shows its drafts there (#0143, `mailbox_list_never_serves_the_server_drafts_folder` in `src/daemon/methods/mailbox.rs`, `the_server_drafts_folder_is_the_mirrors_and_never_a_received_mailbox` in `src/catalogue/tests.rs`); the TUI's sidebar and the desktop's render those rows and add none.

### LST-02 Navigate a list with per-item movement, top and bottom jumps, and half-page scrolling

- Classification: GUI parity
- Source anchor: TUI `j/k`, `gg/G`, `Ctrl+d`, `Ctrl+u` in the EMAIL LIST and BODY keymap sections (`clients/tui/src/app/keymap.rs`)
- Daemon surface: client-side over the list `message.list_stream` returned
- GUI location: clients/desktop: `j`/`k` and the arrows, `gg`/`G`, `Ctrl+d`/`Ctrl+u` in the list, the reader following the selection (M1, #0129); the same keys on the reader scroll the body, inside the HTML frame through its bridge (PERSO-81)
- Validation: TUI golden frames; `clients/desktop/src/keymap/keymap.test.tsx` (`j and k move the selection in the list and the reader follows`, and the block `the reader frame's keys (PERSO-81)`), with no desktop test of the list's jumps or half-page keys
- Status: GUI shipped (M1, #0129)

### LST-03 Jump to a date in the list

- Classification: GUI parity
- Source anchor: TUI `gt` (`clients/tui/src/app/keymap.rs:1276`), `clients/tui/src/app/jump_date.rs`
- Daemon surface: client-side while the list is whole; `message.jump_to_date` if paging lands
- GUI location: clients/desktop (M1, #0131 read slice)
- Validation: unit tests in `clients/tui/src/app/jump_date.rs`
- Status: not started
- Note: accepts relative expressions such as "last week" alongside absolute dates.

### LST-04 Filter the current list by metadata

- Classification: GUI parity
- Source anchor: TUI `fm` (`clients/tui/src/app/keymap.rs:1020`)
- Daemon surface: client-side while the list is whole; `message.filter` if paging lands
- GUI location: clients/desktop (M1, #0131 read slice)
- Validation: TUI golden frames
- Status: not started

### LST-05 Toggle a flagged-only filter

- Classification: GUI parity
- Source anchor: TUI `fF` (`clients/tui/src/app/keymap.rs:1393`)
- Daemon surface: client-side while the list is whole; `message.filter` if paging lands
- GUI location: clients/desktop (M1, #0131 read slice)
- Validation: TUI golden frames
- Status: not started

### LST-06 Search with one query grammar across every backend

- Classification: GUI parity
- Source anchor: `mp search [query] [--mailbox] [field flags] [-n] [--full]`, `SEARCH_LONG_ABOUT` (`src/main.rs:49`), `src/search.rs`, `src/imap_client/search.rs`
- Daemon surface: `message.search` (`--local`); `message.search_server`, a durable `operation.*` streaming hits, for the server leg
- GUI location: clients/desktop (M1, #0131 read slice)
- Validation: unit tests in `src/search.rs`, `tests/cli_help_snapshot.rs` for the grammar's help text; `src/server_search.rs` (`every_mailbox_is_asked_for_the_whole_limit_and_only_the_survivors_download`, `the_newest_stubs_across_mailboxes_survive_the_cut`, `a_mailbox_holding_more_than_it_listed_cuts_the_answer`, `a_refused_mailbox_and_a_dropped_hit_leave_the_rest_alone`, `a_search_across_every_folder_lands_each_hit_under_its_folder`, `a_hit_the_second_pass_cannot_download_is_reported_not_dropped`), `src/daemon/methods/message_server.rs` (`a_target_is_a_configured_mailbox_or_any_server_folder`, `graph_searches_every_folder_at_once_when_no_mailbox_is_named`, `the_first_pass_filters_before_the_limit_is_spent`), `src/graph.rs` (`a_hit_folder_is_labelled_by_its_mailbox_else_its_display_name`, `a_throttled_search_hit_is_asked_again_and_lands`, `a_hit_that_never_comes_back_is_missing_with_why`), `src/read_cmd.rs` (`a_failed_mailbox_is_reported_after_the_answer`, `the_store_coverage_warning_and_the_cut_come_from_the_settle`, `the_cut_line_names_the_hits_listed_not_the_limit`), `tests/daemon_server_leg_slice.rs`
- Status: routed (PERSO-175): `mp search` without `--local` is a client of `message.search_server`, the call the TUI overlay and the desktop search bar make
- Note: `--limit` is the number of hits the whole search answers with: the daemon asks every mailbox for its newest `limit` matches as stubs (UID or Graph id, `Date`, `Message-ID`), merges them newest first, cuts to `limit` and downloads only the survivors. On Graph a search that names no mailbox is one query across every folder, each hit listed under its configured mailbox or its folder's display name. A survivor that could not be downloaded is a failure line, not a silent gap. The settle's `truncated` and `capped` (Microsoft Graph's 1000-result `$search` cap) feed `--json`'s `truncated`, the "raise -n" line and the cap line. The three behaviours that kept the leg in process were settled with Sylvain on 2026-10-22: a mailbox the server refuses is printed as `Search in <mailbox> failed` after the results, from the settle's `unreachable`; `--mailbox` names a configured mailbox or any folder the server lists; and the plain-IMAP `has:attachment` stderr warning, result-header note and JSON fields read the settle's `local_terms`.
- Note: the grammar covers `from:`, `to:`, `cc:`, `subject:`, `body:`, `filename:`, `has:attachment`, `before:`, `after:` with `since:` as an alias, quoted phrases, `OR` groups, `in:`, and `message-id:`; `filename:` resolves only on Gmail, Exchange, or the local index.

### LST-07 Search the local ranked full-text index across every synced mailbox

- Classification: GUI parity
- Source anchor: `mp search --local`, `src/store/search.rs`, `tests/store_search_integration.rs`
- Daemon surface: `message.search`, whose params mirror the command's flags and whose hits come back in the store's ranking order
- GUI location: clients/desktop: Enter in the list header's field searches the selected account's store, and each hit carries its mailbox as a badge (M1, #0129); a query the store's index cannot answer (recipient and message-id terms, an OR group with a date or attachment term, or one mixing a filename with another term), which `message.search` refuses as `-32602`, runs on the server instead, scoped to its `in:` mailbox, with a notice saying why, and a failed search's status wraps rather than cutting the daemon's sentence (PERSO-115)
- Validation: `tests/store_search_integration.rs`, `tests/daemon_read_slice.rs`; `clients/desktop/src/components/search/search.test.tsx` (`runs search_local on Enter and lists the hits with their mailbox`, `opens a hit from another mailbox in the reader`, `a to: query typed in the field goes to the server, which can answer it (PERSO-115)`, `a rerouted query keeps its in: mailbox, so the server searches only that one`, `a refusal reads whole: the status wraps instead of cutting the daemon's sentence`), `clients/desktop/src/app/search.test.ts` ("a header query the store cannot answer"), `clients/desktop/src-tauri/src/commands.rs` (`local_search_flattens_the_row_beside_the_mailbox`, `a_recipient_query_is_refused_by_the_local_search`)
- Status: routed (P4-U4); GUI shipped (M1, #0129)
- Note: `--body` is `body_query` on the wire, because `body` is already the `--full` switch; the client sends what the user typed and the daemon builds the query with `search::from_cli`, so one parser still serves every backend.

### LST-08 Merged search in the TUI

- Classification: GUI parity
- Source anchor: TUI `ff` (`clients/tui/src/app/keymap.rs:723`), `Action::ServerSearch` (`clients/tui/src/app/types.rs:1619`)
- Daemon surface: `message.search` local first, then `message.search_server` as a durable `operation.*` streaming hits on `state.event`
- GUI location: clients/desktop: Shift+Enter, the header's server button and the palette's "Search server" run the server leg, which keeps the hits of a finished local search and excludes their Message-IDs; hits stream in until the settle, and Cancel or leaving the search cancels it (M1, #0129). `ff`, the palette's "Search all mail" and the header's form button open the TUI overlay's form (#0086b) as a dialog: Search In (Current Mailbox or Current Account), From, To, Subject, Keywords, After, Before, the attachment toggle and the Advanced line that greys the rest, with Tab, Space, Enter and Escape as in the TUI; `search_query` builds the query with `mp-core`'s parser, the store answers first when its index can, then the server leg runs over the same scope, and the results fill the list pane (PERSO-115; shell.md, "The Server Search form")
- Validation: TUI golden frames; `the_local_pass_finds_the_row_the_index_holds` (`src/tui_tests/commands.rs`); `clients/desktop/src/components/search/search.test.tsx` (the "server search" and "the Server Search form (PERSO-115)" blocks), `clients/desktop/src/app/search.test.ts`, `clients/desktop/src/keymap/keymap.test.tsx` (`ff opens the Server Search form, the TUI's Search all mail, and the help and palette name it`), `clients/desktop/src-tauri/src/fixture.rs` (`a_server_search_streams_hits_and_finishes`), `clients/desktop/src-tauri/src/commands.rs` (`a_server_search_is_awaited_until_cancelled`, `the_search_form_builds_the_query_the_tui_builds`, `a_recipient_term_is_not_one_the_store_can_answer`)
- Status: routed (P5-U6 local, P5-U10c-I1 server); GUI shipped (M1, #0129), the form (PERSO-115)
- Note: `message.search_server`, deliberately not `message.list_server`, which P4-U10 had given to `mp fetch`'s one-mailbox query, retired with it on 2026-10-22. The TUI's background thread is gone: the overlay appends each hit as its `message.server_hit` event arrives, matched by operation id because a fast retype leaves two searches in flight.
  Deduplication is by Message-ID and it is the daemon's: the client sends the Message-IDs the local pass is showing as `exclude_message_ids` and the settle counts them in `deduplicated`, so a message found twice appears once and the count is a fact any client reproduces.
  The local pass is `message.search` with `body: true` since P5-U6.
  The overlay holds a parsed query and the method takes what a user typed, so the query is rendered back into the grammar (`search::to_query_string`) rather than sent as an engine enum.
  A mailbox the server refuses lands in `unreachable` and does not fail the search; `LST-06`'s CLI leg runs on the same call since PERSO-175.
- Note: the desktop form differs from the overlay in two places. Its results are the list pane's rather than a pane of the dialog, so a hit's keys are `LST-09`'s list keys and the TUI's headers and body panes are the reader; and Search In stays live while the Advanced line greys the other fields, since the TUI still applies the scope to an Advanced query it greys. The desktop's `search_query` repeats the TUI's `SearchForm::build_query` over `mp-core`, since no crate under `crates/` holds the form's lowering yet.

### LST-09 Act on a search result without leaving the overlay

- Classification: GUI parity
- Source anchor: the SERVER SEARCH keymap section (`clients/tui/src/app/keymap.rs`): `Enter`, `e`, `y`, `f`, `r`, `R`, `w`, `cF`, `te`, `a`, `b`, `o`, `O`
- Daemon surface: `message.get`, `message.materialise_html`, `message.materialise_attachment`, `message.materialise_eml`, `message.fetch`, `message.archive`, `draft.reply`, `draft.forward`
- GUI location: clients/desktop (M1 for open and copy, #0129); archive is `a` on a result with a local row, which asks first like a list row and takes the hit out of the results (M2, #0131); a hit is a list row, so `r`, `ca` and `cf` reply and forward (a server-only hit's reply through `draft_from_message`, its forward through the wizard and `draft_forward` with the hit as its source, as the overlay's `w`, PERSO-155), `tb`, `to` and `ts` stand for the overlay's `b`, `o` and `O`, a server-only hit's parts read from the server's copy through the daemon's hit address, `cF` and `te` and the hit's Forward as attachment and Export .eml buttons are the overlay's own `cF` and `te` (PERSO-174), and `F` or the reader's Fetch button fetches a server-only hit into the store and opens its row (M3, #0131; shell.md, "Compose", and reader.md, "Drafts and server-only hits")
- Validation: TUI golden frames; `src/tui_tests/actions_store.rs` (`a_server_only_hits_attachments_and_view_come_from_the_daemon`, `the_search_overlays_forward_opens_the_wizard_and_cancel_returns_to_it`), `src/tui_tests/actions.rs` (`an_account_wide_search_sends_the_desktops_request`), `tests/daemon_eml_export.rs` (`a_server_search_hit_is_read_and_materialised_through_its_message_id`); the desktop's archive of a hit runs the list's path (`actionTargets` over the results), with no search-specific test; `clients/desktop/src/keymap/keymap.test.tsx` (`on a server-only hit, r and ca build the draft from the hit's own headers, and cf and cF ask for the recipients first (PERSO-155)`, `on a server-only hit, e, to and ts read the server's copy through the hit's address, with no fetch first (PERSO-155)`), `clients/desktop/src-tauri/src/attachments.rs` (`a_server_only_hits_parts_are_listed_opened_and_saved`), `clients/desktop/src/components/reader/reader.test.tsx` (`F fetches it into the store, then the reader opens the row and a second F says it is there`, `the Fetch button fetches, and a refusal lands in the activity area`, `opens the hit's own markup in the browser, and one with none has no button`), `clients/desktop/src-tauri/src/commands.rs` (`fetch_ingests_a_server_only_message_once_and_then_says_it_is_present`)
- Status: routed (P5-U6, completed by P5-U10c-I1); GUI shipped (M2, #0131) for archive, and (M3, #0131) for reply, reply-all, forward, fetch, the browser rendition and attachments; the rest not started
- Note: a server-only hit has no row to archive or to put the list cursor on, so `a` and the TUI's Enter say to fetch it first; every other key reads it through the daemon's `hit: {message_id, mailbox}` address, `message.get`, `message.materialise_attachment`, `message.materialise_markdown`, `message.materialise_eml` and `draft.forward`, which answer from the server's copy without ingesting it (PERSO-155).
  An account-wide search names no mailbox in all three clients, so the daemon runs its own account-wide search, one query across every folder on Graph (PERSO-155).
  Its `.eml` needs no fetch: `cF` and `te` (PERSO-174) address it as `f` does, `{message_id, mailbox}`, through `draft.forward`'s hit source and `message.materialise_eml`'s `hit`, and the daemon downloads the server's raw copy without ingesting it, so the hit stays a hit.
  Since #0141 a hit from any folder the server lists is fetchable, stored under that folder's own key whatever its policy (`a_hit_from_any_server_folder_is_fetchable` in `src/daemon/methods/message_server.rs`).
  The desktop fetches with `F`, because `f` is the find family's prefix there (`fm`, `ff`), and replies to all with `ca` rather than the overlay's `R`.
- Note: the overlay's reply, forward, archive, browser rendition and attachment keys are daemon methods since P5-U6; `f` and the three rendition keys joined them in P5-U10c-I1.
  `f` is `message.fetch` `{account, mailbox, message_id}`, a durable operation that is idempotent over a message the store already holds: it answers with that row and `already_present: true` and opens no session, so the overlay keeps its "Already in the local store" line by branching on a boolean rather than matching a refusal.
  `Enter` / `e` / `y` are `RD-06`'s `message.materialise_markdown`.
  One key still reads the store inline, and it is not this row's: `Action::OpenEventSource` on the agenda hands `$EDITOR` a row's `invite.ics` blob, which no `message.*` method serves.

### LST-10 Show the conversation a message belongs to

- Classification: GUI parity
- Source anchor: TUI `tt` (`clients/tui/src/app/keymap.rs:1073`), the thread overlay in `clients/tui/src/ui/overlays.rs`
- Daemon surface: `message.thread`
- GUI location: clients/desktop (M1, #0131 read slice)
- Validation: TUI golden frames, `tests/daemon_thread_slice.rs`
- Status: routed (P5-U10d-I); GUI not started
- Note: `open_thread_overlay` asked `message.thread` on the session the `App` holds since P5-U10d-I, where it opened the store, read the row and folded `read::thread_messages` over it; it was the last read in `clients/tui/src/app/` that no method answered.
  The method takes `message.get`'s address and answers `{account, thread_id, subject, messages}`, the rows oldest first and one per `Message-ID`; a thread row carries the `mailbox` its copy lives in, which a listing row does not, because a conversation crosses mailboxes and the overlay's `Enter` switches to the one it opens.
  A message with no relatives answers with itself alone, and the client keeps its "No related emails for this message in the store" line by branching on the length.

### LST-14 List a mailbox one row per conversation

- Classification: GUI parity
- Source anchor: the TUI's conversation list (#0142): `list_mailbox` and the header and member rows in `clients/tui/src/queries.rs` and `clients/tui/src/ui/list.rs`, `l` / `h` (`ExpandConversation`, `CollapseConversation`) and `tc` (`ToggleConversationMode`) in `clients/tui/src/app/keymap.rs`, the per-account choice in `tui.json` (`clients/tui/src/prefs.rs`)
- Daemon surface: `message.list_conversations` (streamed on `message.conversations`, the transport of `message.list_stream`), `message.thread` with `prefer_mailbox` and `include_drafts` for an expansion, the per-message mutations for an action on a row; both clients require `message.list_conversations` at the handshake
- GUI location: clients/desktop: `ConversationRow` and `ConversationMemberRow` in the list pane, `Enter`, `l` and `h` from the generated `keymap.json`, `tc` and its palette entry, and a "Conversation view" switch on each account's Settings card, stored in `prefs.flatListAccounts` (#0142; shell.md, "Model" and "Settings")
- Validation: `tests/daemon_conversations.rs` (`streamed_conversations_equal_the_fold_of_the_listing_and_the_thread`, `an_inbox_message_its_two_sent_replies_and_a_draft_are_one_conversation`, `a_note_to_self_in_the_inbox_is_the_users_own`, `a_sent_copy_from_another_address_is_the_users_own`), `tests/daemon_conversations_cap.rs` (`no_conversation_frame_passes_the_budget_on_the_large_mailbox`), `src/tui_tests/conversations.rs` (golden frames `golden_conversation_collapsed` and `golden_conversation_expanded`, `actions_on_a_collapsed_row_reach_its_members_here`, `deltas_restream_the_list_coalesced`, `tc_and_the_palette_switch_the_list_and_tui_json_remembers_it`, `the_users_own_members_read_me_with_a_tinted_label`, `a_wide_member_puts_its_sender_under_contact_and_its_mailbox_under_subject`, `an_expanded_conversation_reads_as_one_block_with_a_visible_end`), `clients/tui/src/prefs.rs` (the `tui.json` round trip and its malformed-file fallback), `clients/desktop/src/app/conversations.test.ts`, `clients/desktop/src/components/list/conversations.test.tsx` (the "conversation view setting" suite: `tc` and a restart; `tells the user's own members apart: a tinted mailbox chip and Me, with the address it went out from`, `draws an expanded conversation as one block: a tinted header, indented members on a lighter tint, a rule after the last`), `clients/desktop/src/components/settings/settings.test.tsx` (`shows each account's choice and flips it, stored with the prefs`), `clients/desktop/src-tauri/src/commands.rs` (`conversation_mode_lists_and_expands_the_canned_thread`)
- Status: routed (#0142 steps 2 and 3); TUI and GUI shipped (#0142 steps 4 to 6), on by default for every account the client's stored list does not name
- Note: `LST-10` stays the `T` overlay of one conversation; this row is the listing that groups a whole mailbox, and the Drafts mailbox stays the flat `draft.list` in both clients.
  The choice is presentation and stays in each client, `tui.json` beside config.toml for the TUI and localStorage for the desktop, so the two may disagree for one account as their themes may.
  Graph rows and replies sent before PERSO-137 root their own conversations until #0142's step 7 threads them by the server's ids.

### LST-11 Legacy server fetch with inline filters

- Classification: diagnostics and maintenance
- Source anchor: `mp fetch [--from --to --cc --subject --body --since --before -n --full --mailbox]`, removed from `src/main.rs`
- Daemon surface: none; `message.list_server` was retired with it
- GUI location: not required
- Validation: `tests/daemon_sync_slice.rs` (`mp_fetch_is_retired`), `tests/daemon_server_leg_slice.rs` (`the_daemon_advertises_both_server_leg_methods`), `tests/cli_help_snapshot.rs`
- Status: retired (2026-10-22)
- Note: a duplicate of `mp search --mailbox <name>` with the same filters (`--from`, `--to`, `--cc`, `--subject`, `--body`, `--after`/`--before`, `-n`, `--full`), which runs on `message.search_server` (`LST-06`); Sylvain decided its retirement on 2026-10-22 (`ANO-3`). `mp fetch` with an argument now fails as an unknown argument, and `mp fetch` alone is read as a draft selector, the dry run every bare word is.

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
- Source anchor: TUI `e` (`clients/tui/src/app/keymap.rs:915`), search overlay `e`; `Enter` on a list row focuses the body pane since PERSO-92, as in the desktop client, so `e` alone opens `$EDITOR`
- Daemon surface: `message.materialise_markdown` `{account, row_id|id|selector, mailbox?}`, the third member of the handle family
- GUI location: clients/desktop: `e` on a received message, in a mailbox or on a stored search hit, opens the rendition by a draft's route: with a terminal editor `message_markdown_path` materialises it and `terminal_spawn` runs the editor on it in the reader area's embedded pane, which closes when the editor exits, and with a GUI editor `message_markdown_open` opens it in the external editor; Enter focuses the reader, as in the TUI the reader shows on Enter; on a draft `e` still edits it, and on a server-only hit it opens the daemon's rendition of the server's copy, as the TUI overlay's `e` does (PERSO-92, PERSO-155; shell.md, "Compose", "A message's rendition")
- Validation: TUI golden frames, `clients/tui/src/app/keys.rs` (`e_on_a_received_row_opens_the_editor`, `e_in_the_body_pane_opens_the_editor`, `a_draft_keeps_e_for_editing_and_enter_reads_it`); `clients/desktop/src/keymap/keymap.test.tsx` (`e on a message opens its Markdown rendition in the editor and Enter focuses the reader; on a draft e edits it (PERSO-92)`, `on a server-only hit, e, to and ts read the server's copy through the hit's address, with no fetch first (PERSO-155)`), `clients/desktop/src/components/compose/embedded.test.tsx` (`e on a received message opens its read-only rendition in the reader area like a draft, and code 0 brings the message back (PERSO-92)`, `a rendition's editor that fails keeps its pane, says nothing of a save, and Reopen writes the rendition again (PERSO-92)`, `0 from a message's rendition gives the focus back to the list too (PERSO-124)`), `clients/tui/src/actions.rs` (`a_clean_return_from_e_puts_the_keyboard_back_on_the_list`), `clients/desktop/src-tauri/src/daemon_files.rs` (`message_markdown_path_on_answers_the_read_only_rendition_and_opens_nothing`, `message_markdown_open_on_journals_the_editor_on_the_daemons_rendition`, `message_markdown_open_on_a_row_the_store_lost_is_not_found_and_opens_nothing`, `message_markdown_path_on_a_server_only_hit_is_the_daemons_rendition`)
- Status: routed (P5-U10c-I1); GUI shipped (PERSO-92)
- Note: the handle keeps its blob alive until release or expiry (`ANO-6`), and the file is written 0444 so `$EDITOR` opens it read-only (#0075).
  The desktop leaves the handle to expire, as it does an opened attachment's, on either route; the TUI releases it on exit.
  The embedded pane's Reopen materialises the rendition again, since the first file may have expired.
  A terminal editor opens in a new terminal window, never in the embedded pane, which is a draft's.
  The rendition is released when the editor exits, where the pre-daemon file was unlinked; a second open of the same row is a second handle in a directory of its own, so the mode the first one left cannot stop it.
  `message.materialise_html` is not it: that renders the sender's markup for a browser, where this renders the store's own Markdown view of a message.
  The store keeps no `.md` per message, so the rendition is `store::read::render_markdown` on every call, which is what makes a handle the right shape rather than a path.
  It is the one member of the family that takes `row_id`, because the TUI holds a `MessageRef` and nothing else (#0050).

### RD-07 Copy a message's `mp://` selector to the clipboard

- Classification: GUI parity
- Source anchor: TUI `y` (`clients/tui/src/app/keymap.rs:980`)
- Daemon surface: `selector` on the `message.list` row and on the `message.search` hit, then a client-side clipboard write
- GUI location: clients/desktop: `y` copies the selected message's `mp://` selector and says "Copied <selector>" (M1, #0129), and on a draft in Drafts its file path instead, listed as "Copy the draft's file path (Drafts only)" (PERSO-100), where the TUI copies the draft's selector; the copy goes through `copyText`, and the reader's Copy menu and the palette's "Copy link (mp://)" copy the same selector (M4, #0131; reader.md, "The toolbar")
- Validation: `tests/cli_selector_contract.rs` for the selector shape; `clients/desktop/src/lib/clipboard.test.ts` (`writes the text and says what it copied`), `clients/desktop/src/components/activity/activity.test.tsx` (`the Copy menu copies the sender's address, the mp:// link and the subject`), `clients/desktop/src/keymap/keymap.test.tsx` (`y on a draft copies its file path, and on a message its mp:// selector`)
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
- GUI location: clients/desktop, not built in M1 to M4: the palette lists the search overlay's "Copy the Markdown rendition path" with the badge "later"; the desktop materialises the rendition for `e` only (`RD-06`); a hit is a list row there, so `y` copies its selector, and the rendition's path waits for a clipboard write that survives the call's await (`copyText` must run inside the key press)
- Validation: TUI golden frames
- Status: routed (P5-U10c-I1); GUI not started
- Note: the path a `y` copies now names a file inside a handle directory, which the family releases after ten minutes: nothing reads a yanked path back, so what changed is how long a pasted one resolves.

## Selection and message actions

### MSG-01 Archive a received message on the server and locally

- Classification: GUI parity
- Source anchor: `mp archive <selector> [--mailbox]` (`src/main.rs`), TUI `a` (`clients/tui/src/app/keymap.rs:935`)
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
  A discarded draft moves into `drafts/.retired/` and, on a mirrored account, its server copy is queued for deletion (#0143, `a_retired_draft_forgets_its_row_and_queues_the_delete` in `src/draft_mirror/tests.rs`).
  The desktop never forces, so an approved draft is refused and put back, and it has no `--sent` sweep.

### MSG-03 Toggle read and unread

- Classification: GUI parity
- Source anchor: TUI `u` (`clients/tui/src/app/keymap.rs:953`)
- Daemon surface: `message.set_read` `{account, row_id, read, settle}`
- GUI location: clients/desktop: `u` in the list or the reader, the row's "Unread" toggle, the reader toolbar and the palette; over marks, marking read wins when any marked row is unread (M2, #0131)
- Validation: `src/tui_tests/actions.rs`, TUI golden frames; `clients/desktop/src/components/mutations/mutation-ui.test.tsx` (`the flag and unread toggles act on their own row`), `clients/desktop/src/app/reducer.test.ts` (`marks read in place and moves the unread count`), `clients/desktop/src-tauri/src/commands.rs` (`flag_and_read_set_the_state_they_name`)
- Status: routed (P5-U6); GUI shipped (M2, #0131)
- Note: the daemon takes the new state rather than a toggle, and the TUI sends `settle: false`, so the row change and the owed `SetRead` commit together (#0039) and the account's drainer drains them once mutations have been quiet for 1.5 s (#0133).

### MSG-04 Toggle the `\Flagged` star

- Classification: GUI parity
- Source anchor: TUI `*` (`clients/tui/src/app/keymap.rs:962`)
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
- Source anchor: TUI `v` (`clients/tui/src/app/keymap.rs:1287`), `Ctrl+a` (`clients/tui/src/app/keymap.rs:1298`), the batch actions in `clients/tui/src/app/types.rs`
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
- Source anchor: `mark_open_read` (`clients/tui/src/commands.rs:1445`), reached from the received-row branch of `Action::EditCurrent` and from `Action::MarkAsRead`, which `queue_mark_open_read` (defined at `clients/tui/src/app/mod.rs:1230`, pushed from `clients/tui/src/app/keys.rs:322`, `:336` and `:560`) queues on a focus move into the body pane, `Tab`, `Shift+Tab` or `Enter` (PERSO-92)
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
- Source anchor: `mp new <name>` (`src/main.rs`), TUI `cn` (`clients/tui/src/app/keymap.rs:733`)
- Daemon surface: `draft.create`
- GUI location: clients/desktop: `cn` from any pane or the palette's "New draft" opens the wizard, whose submit calls `draft_create` with the recipients, the signature and any inline body, and opens the file in the editor unless a body was typed (M3, #0131; shell.md, "Compose", "The wizard")
- Validation: `tests/draft_integration.rs`, `tests/daemon_draft_slice.rs`; `clients/desktop/src/keymap/keymap.test.tsx` (`cn opens the new-draft wizard for the shown account`), `clients/desktop/src/components/compose/compose.test.tsx` (`Enter moves to the next field, and Cmd+Enter creates the draft and opens it in the editor`, `shows the daemon's refusal in the dialog, which stays open`), `clients/desktop/src-tauri/src/commands.rs` (`a_created_draft_is_a_file_with_frontmatter_and_a_taken_name_is_refused`); the focus after the editor: `clients/tui/src/actions.rs` (`a_clean_return_from_a_new_drafts_editor_puts_the_keyboard_back_on_the_list`)
- Status: routed (P4-U6); GUI shipped (M3, #0131)
- Note: the desktop prints no selector; the new draft's file opens in the editor and its row appears in Drafts through `draft.changed`.
- Note: as after `e` (DFT-07), an editor that returns cleanly gives the keyboard back to the list in both clients, the TUI's `cn` from the headers or body pane it was pressed in and onto the list it was pressed over, since it does not switch to Drafts; a failed editor keeps the focus, and a body typed in the wizard opens no editor and moves nothing (PERSO-129).

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
- Source anchor: `mp mark-approved`, `mp mark-draft` (`src/main.rs`), TUI `cA` and `cD` (`clients/tui/src/app/keymap.rs:1360-1370`)
- Daemon surface: `draft.approve`, `draft.demote`, resolved through `draft.path` first so the client knows the previous status
- GUI location: clients/desktop: `cA` and `cD` in Drafts, the palette and the draft preview's Approve and Back to draft buttons, on the marked drafts after a confirmation or on the cursor draft at once; the status changes in the list as a pending axis, and a draft that does not parse is refused alone with the diagnostic and the path (M3, #0131; shell.md, "Approve and demote")
- Validation: `tests/draft_integration.rs`, `tests/daemon_draft_slice.rs`; `clients/desktop/src/keymap/keymap.test.tsx` (`cA approves the cursor draft and cD puts it back`, `cA over marked drafts asks first, with the TUI's words, and approves the batch`), `clients/desktop/src/components/compose/compose.test.tsx` (`a draft that does not parse is refused, and the alert names why and where`), `clients/desktop/src-tauri/src/commands.rs` (`approve_refuses_an_invalid_draft_with_its_payload_and_goes_on`)
- Status: routed (P4-U6); GUI shipped (M3, #0131)

### DFT-05 Preview a draft as a dry run through a bare selector

- Classification: GUI parity
- Source anchor: the top-level positional `[SELECTOR]` argument in `src/main.rs`
- Daemon surface: `draft.preview`
- GUI location: clients/desktop: Enter on a draft shows `DraftPreview` in the reader, with From, To, Cc, Bcc, the status, the whole body (`draft.preview` with `full: true`, where the dry run cuts it at 500 characters) and the file path (M3, #0131; PERSO-101; reader.md, "Drafts and server-only hits")
- Validation: `tests/cli_selector_contract.rs`, `tests/mime_oracle_integration.rs`, `tests/daemon_draft_slice.rs` (`draft_preview_full_carries_the_whole_body`); `clients/desktop/src-tauri/src/commands.rs` (`the_draft_preview_carries_the_whole_body`); `clients/desktop/src/components/compose/compose.test.tsx` (`Enter opens a draft's preview in the reader`, `shows the headers, the status, the validation and the body of the selected draft`)
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
- Validation: `tests/daemon_draft_slice.rs`, with a stub editor that records the path it was handed; `clients/desktop/src/keymap/keymap.test.tsx` (`e on a draft resolves its path and opens the editor; on a message it opens the reader`), `clients/desktop/src/components/compose/compose.test.tsx` (`names the draft and the editor; Reopen runs the editor again and Done ends the session`, `an editor that did not start is a failure notice naming what to set`), `clients/desktop/src-tauri/src/editor.rs` (`the_env_and_the_setting_win_over_visual_editor_and_the_probes`, `a_terminal_editor_in_the_environment_falls_through_to_the_probes`); the focus after the editor: `clients/desktop/src/components/compose/embedded.test.tsx` (`0 from a draft edited in the Drafts list gives the focus back to the list, on the draft's row (PERSO-124)`, `a nonzero code keeps the focus on the reader, where the pane and its output stay (PERSO-124)`), `clients/tui/src/actions.rs` (`a_clean_return_from_e_puts_the_keyboard_back_on_the_list`)
- Status: routed (P4-U6); GUI shipped (M3, #0131) through the external editor, and (M5, #0130) through the embedded session for a terminal editor
- Note: a terminal editor (`nvim`, `vim`, `hx`, ...) named by `MP_DESKTOP_EDITOR`, the `editor` setting, `$VISUAL` or `$EDITOR` opens the draft in the embedded terminal pane on the same file (shell.md, "The embedded editor"; rust-layer.md, "Terminal sessions"); a GUI editor, a probed `code`, `zed`, `subl` or `cursor`, or `open -t` keeps the external route, in that order.
- Note: an editor that returns cleanly (code 0, `:wq`) gives the keyboard back to the list in both clients, the desktop from the reader that showed the embedded editor and the TUI from the headers or body pane `e` was pressed in (PERSO-124).

### DFT-08 Create a reply or a reply-all draft from a received message

- Classification: GUI parity
- Source anchor: `mp reply <selector> [--all] [--mailbox]` (`src/main.rs`), TUI `r`, `cr` (`clients/tui/src/app/keymap.rs:1030`), `ca`, search overlay `r` and `R`
- Daemon surface: `draft.reply`, and `draft.create_from_message` for a hit with no local row
- GUI location: clients/desktop: `r` or `cr` and `ca` from the list or the reader, the reader toolbar's Reply and Reply all, and the palette, which open the new draft in the external editor; on a server-only hit they go through `draft_from_message` (M3, #0131; shell.md, "Compose")
- Validation: `tests/draft_integration.rs`, `tests/daemon_draft_slice.rs`, `tests/daemon_draft_from_message_slice.rs`; `clients/desktop/src/keymap/keymap.test.tsx` (`r and cr reply to the cursor row and open the draft in the editor, ca replies to all`, `on a server-only hit, r and ca build the draft from the hit's own headers, and cf and cF ask for the recipients first (PERSO-155)`), `clients/desktop/src/components/compose/compose.test.tsx` (`Reply, Reply all and Forward act on the open message`), `clients/desktop/src-tauri/src/commands.rs` (`a_reply_carries_in_reply_to_and_the_subject`); the focus after the editor: `clients/tui/src/actions.rs` (`a_clean_return_from_a_new_drafts_editor_puts_the_keyboard_back_on_the_list`)
- Status: routed (P5-U6, and P5-U10d-I for the hit with no row); GUI shipped (M3, #0131)
- Note: the TUI's four reply keys went through it in P5-U6, addressed by the `row_id` the method gained for them.
  A search hit that resolved to no local row is the one reply neither `draft.reply` nor `draft.forward` can build, because every form of their `source` is an address into the store; P5-U10d-I routed it through `draft.create_from_message` `{account, kind, message}`, where the message travels instead of an address, nothing is ingested, no unread count moves, and a hit with no `Message-ID` or in a mailbox the sidebar does not list still quotes.
- Note: as after `e` (DFT-07), an editor that returns cleanly gives the keyboard back to the list in both clients, the TUI's `r`, `cr` and `ca` from the headers or body pane they were pressed in; a failed editor keeps the focus, and the search overlay's `r` and `R` leave the focus under the overlay alone, as its `e` does (PERSO-129).

### DFT-09 Forward a message to new recipients

- Classification: GUI parity
- Source anchor: `mp forward <selector> [--mailbox] [--as-attachment]` (`src/main.rs`), TUI `cf` and `cF`, search overlay `w` and `cF`
- Daemon surface: `draft.forward`, with `as_attachment` for a forward as attachment, and `draft.create_from_message` for a hit with no local row
- GUI location: clients/desktop: `cf` from the list or the reader, the reader toolbar's Forward and the palette open the forward wizard with To, Cc, Bcc and a `Fwd: ` subject, whose submit calls `draft_forward` with `headers` and opens the draft in the external editor; a server-only hit opens the same wizard, whose submit is `draft_forward` with the hit as its source, as the TUI overlay's `w` and `cF` open it over any hit and return to the overlay (M3, #0131, PERSO-155; shell.md, "The wizard")
- Validation: `tests/draft_integration.rs`, `tests/mime_oracle_integration.rs`, `tests/daemon_draft_slice.rs`, `tests/daemon_draft_from_message_slice.rs`; `clients/desktop/src/keymap/keymap.test.tsx` (`cf on a stored message asks for the recipients first, with the forward's subject`), `clients/desktop/src-tauri/src/commands.rs` (`a_forward_carries_the_attachments_and_a_hit_is_built_from_itself`); the focus after the editor: `clients/tui/src/actions.rs` (`a_clean_return_from_a_new_drafts_editor_puts_the_keyboard_back_on_the_list`)
- Status: routed (P5-U6, and P5-U10d-I for the hit with no row); GUI shipped (M3, #0131)
- Note: the forward carries the original attachments, which the GUI must reproduce rather than dropping.
  A forward of a server-only hit carries the server copy's parts: `draft.forward` takes the hit as its source and the daemon builds the forward from the server's copy, ingesting nothing (PERSO-155), where `draft.create_from_message`, which a reply to a hit still goes through, takes none.
  P5-U6 routed the TUI's two forward keys through it and gave the method `headers`, the compose wizard's override of the recipients and the subject it collected before the draft existed.
- Note: as after `e` (DFT-07), an editor that returns cleanly gives the keyboard back to the list in both clients, the TUI's `cf` from the headers or body pane it was pressed in; a failed editor keeps the focus, and the search overlay's `w` leaves the focus under the overlay alone (PERSO-129).
- Note: a forward as attachment (PERSO-174) carries the source as one `<date>_<subject>.eml` part, labelled `message/rfc822` with `7bit` or `8bit`, instead of its parts: `mp forward --as-attachment`, the TUI's `cF` in the list and in the search overlay, and the desktop's `cF`, reader toolbar button and palette entry "Forward as attachment (.eml)", the desktop's through the forward wizard titled "Forward as attachment" for a stored message, all calling `draft.forward` with `as_attachment: true`.
  The `.eml` is `message.materialise_eml`'s bytes, so a Graph row is fetched from the server first; a server-only hit has no row and is `draft.forward`'s `source: {hit: {message_id, mailbox}}`, whose `.eml` the daemon downloads without ingesting it, and whose forward it builds from those bytes (the desktop's `draft_forward` with a hit `source`).
  The body is the usual forward's; `mp_core::draft::ATTACHED_FORWARD_BODY` is the one line that would make it Outlook's, the signature alone.
  A server whose EHLO offers no `8BITMIME` gets an `8bit` part as base64 `application/octet-stream` under the same name: lettre refuses the 8bit bytes before `MAIL FROM`, so the send retries once with the 7bit bytes and nothing goes out twice.
  Validation: `tests/daemon_eml_export.rs` (with `a_server_search_hit_forwards_as_attachment`), `src/send.rs` (`an_eml_with_long_8bit_lines_goes_8bit_byte_for_byte`, `an_eml_with_a_line_over_998_octets_goes_as_an_octet_stream`, `a_server_without_8bitmime_gets_the_eml_as_7bit_exactly_once`), `src/tui_tests/actions_store.rs` (`cf_and_cf_upper_open_the_forward_wizard_with_and_without_the_eml`, `the_search_overlays_te_exports_a_server_only_hits_eml`), `clients/tui/src/commands.rs` (`a_forward_as_attachment_is_draft_forward_with_as_attachment`, `a_hit_with_no_row_forwards_through_its_message_id`), `tests/daemon_eml_export.rs` (`a_server_search_hit_forwards_inline_with_the_wizards_headers`), `clients/desktop/src-tauri/src/commands.rs` (`a_server_only_hit_forwards_inline_with_the_wizards_headers`), `clients/desktop/src/keymap/keymap.test.tsx` (`cF forwards as attachment through the same wizard, draft_forward with as_attachment (PERSO-174)`), `clients/desktop/src-tauri/src/commands.rs` (`a_forward_as_attachment_carries_the_eml_instead_of_the_parts`).

### DFT-10 Compose wizard for new and forwarded mail

- Classification: GUI parity
- Source anchor: the compose wizard variants in `clients/tui/src/app/types.rs`, `clients/tui/src/ui/compose.rs`
- Daemon surface: `draft.create` with `headers`, `body` and `signature` or `no_signature`, `draft.forward` with `headers`, `signature.list`, `signature.read`, `signature.create`, `contact.search` for the recipient completion; the wizard itself is client-side
- GUI location: clients/desktop: `ComposeWizard`, a dialog opened by `cn` ("New draft") and `cf` ("Forward"), with To, Cc, Bcc and Subject, a Signature select, an inline Body for a new draft, which skips the editor when it is filled, as in the TUI, Enter to the next field, Cmd+Enter or Ctrl+Enter to submit and Escape to cancel; it needs one recipient, as the TUI's does (M3, #0131; shell.md, "The wizard")
- Validation: TUI golden frames; `clients/desktop/src/components/compose/compose.test.tsx` (`has the TUI's fields, the default signature preselected, and a none option`, `Enter moves to the next field, and Cmd+Enter creates the draft and opens it in the editor`, `refuses a draft with no recipient, as the TUI does, and stays open`, `Escape cancels and writes nothing`), `clients/desktop/src/components/shell/a11y.test.tsx` (`names the new-draft wizard, starts it in To, and keeps Tab inside it`), `clients/desktop/src-tauri/src/fixture.rs` (`a_created_draft_records_the_signature_it_was_asked_for`, the `signature:` field the daemon writes for a named signature)
- Status: routed (P5-U6) for its forward mode and since the daemon leftovers' TUI follow-up (2026-10-22) for its new mode, which wrote the draft file itself until then; GUI shipped (M3, #0131), with the inline body since `draft.create` took `body` and `headers` (#0131, "Daemon gaps closed")
- Note: an inline body field, a signature picker, and a submit chord; the overlay-internal keys go into `docs/baselines/pre-daemon/manual-keys.md`, the P0-U2 inventory (`ANO-2`).

### DFT-11 Edit the recipients of an existing draft

- Classification: GUI parity
- Source anchor: TUI `ce` in the drafts mailbox (`clients/tui/src/app/keymap.rs:1330`)
- Daemon surface: `draft.set_headers`, which rewrites the fields it names and re-splices the signature block when asked
- GUI location: clients/desktop: `ce` in Drafts, the palette and the draft preview's Edit recipients open the "Edit recipients" dialog filled from the listed draft row, Bcc included, and its submit calls `draft_set_recipients`, which is `draft.set_headers` with the subject left out when it did not change (M3, #0131; shell.md, "The recipients dialog")
- Validation: TUI golden frames; `edit_recipients_rewrites_the_headers_through_the_daemon` (`src/tui_tests/actions_store.rs`), `a_header_edit_asks_the_daemon_for_the_fields_and_the_resplice` (`clients/tui/src/actions.rs`); `tests/daemon_gui_gaps.rs` (`a_drafts_headers_are_rewritten_by_the_daemon`); `clients/desktop/src/keymap/keymap.test.tsx` (`ce opens the recipients dialog filled from the listed draft, Bcc included`), `clients/desktop/src/components/compose/compose.test.tsx` (`rewrites the recipients and keeps the subject when it did not change`), `clients/desktop/src-tauri/src/commands.rs` (`set_recipients_rewrites_the_header_and_keeps_the_body`)
- Status: GUI shipped (M3, #0131) for the recipients and the subject; the dialog has no signature select, so it never re-splices the signature the TUI's `ce` re-splices when it changed
- Note: neither client writes the file: both call `draft.set_headers`, and the TUI's `ce` passes `splice_signature` when the wizard's signature changed.
  The write is the daemon's, so a mirrored draft's header edit is pushed like any other change to the file (#0143).

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

### DFT-13 Mirror drafts to the server Drafts folder

- Classification: daemon behaviour, shown in both clients
- Source anchor: `src/draft_mirror/` (push, delete, the owed-push scan), `src/store/draft_sync.rs`, the watcher's `MirrorState` in `src/daemon/watch.rs`; [#0143](tickets/0143-server-drafts.md)
- Daemon surface: no method; the `server` member `{state, synced_at}` of a snapshot draft row, a `draft.changed` payload and a `draft.list` row, and the queued `push_draft` and `delete_draft` ops of the account's mutation drain
- GUI location: clients/desktop: a Drafts row shows a `synced`, `pending`, `conflict` or `failed` badge and names it in the row's accessible name; a draft that is not mirrored shows none; TUI: the Drafts list leads a mirrored row with a cloud, upload, fork or bolt glyph, and the header pane's Date line reads `[draft, synced]`
- Validation: `src/draft_mirror/tests.rs` (the push, the replace, the owed-push scan, the delete, the drain and its failure, and the reconcile: `a_lost_row_is_recovered_from_the_copy_that_is_the_file`, `an_outlook_edit_is_pulled_above_the_quote_and_demotes_an_approval`, `an_edit_on_both_sides_keeps_the_local_text_as_a_conflict_copy`, `a_copy_gone_elsewhere_retires_the_draft`, `an_apple_mail_edit_under_a_new_message_id_is_relinked`, `a_draft_from_the_other_machine_is_imported_under_its_id`, `a_retired_drafts_copy_is_deleted_and_a_hand_removed_one_is_kept`, `a_crash_between_append_and_delete_leaves_one_copy_after_the_reconcile`, `a_draft_started_in_outlook_is_imported_as_a_markdown_file`, `a_foreign_reply_keeps_its_quote_below_a_marker`), `crates/mp-core/src/parse.rs` (`a_foreign_drafts_html_converts_to_draft_markdown`, `the_reply_boundary_is_the_first_quote_wrapper`, `a_drafts_source_comment_round_trips`), `tests/daemon_draft_watch.rs` (`a_draft_is_described_again_with_its_server_state_once_the_mirror_is_read`, `conflicts_failures_and_a_switched_off_mirror_have_their_own_states`); `clients/tui/src/ui/list.rs` (`a_mirrored_draft_shows_its_server_state`), `clients/tui/src/ui/headers.rs` (`a_mirrored_drafts_date_line_names_its_server_state`), `clients/tui/src/app/types.rs` (`a_listed_drafts_server_state_reaches_its_row`); `clients/desktop/src/components/list/draftRow.test.tsx`
- Status: daemon and both clients shipped (#0143 steps 2 to 6: the push, the delete of the copy after a send to every recipient and after a discard, and the reconcile after every sync with its pull, conflict copy, retirement, row recovery, and the import of a draft mp pushed elsewhere or started in another client); the Graph half (step 7) is not built
- Note: `[accounts.drafts] mirror = false` keeps an account's drafts local, and a Graph account is not mirrored until the Graph half is built.

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

### ATT-06 Export a received message as its RFC 822 file

- Classification: GUI parity
- Source anchor: `mp export <selector> [-o dir] [--mailbox]` (`src/main.rs`), TUI `te` (`clients/tui/src/app/keymap.rs`)
- Daemon surface: `message.materialise_eml`, plus a client-side copy into the destination; the CLI and the desktop release the handle, the TUI leaves it to its lifetime as its `ts` does
- GUI location: clients/desktop: `te` from the list or the reader, the reader toolbar's Export .eml, or the palette's "Export message as .eml", opens the "Export message" dialog with the Save dialog's Directory field, Browse and last directory; `message_export` copies with the `_1` rule and releases the handle (PERSO-174; reader.md, "Attachments")
- Validation: `tests/daemon_eml_export.rs` (with `a_server_search_hit_is_exported_through_its_message_id`); `crates/mp-core/src/parse.rs` (`a_long_cjk_or_emoji_subject_is_cut_by_bytes_on_a_char_boundary`); `src/tui_tests/actions_store.rs` (`te_hands_the_messages_eml_to_the_directory_picker`, `the_search_overlays_te_exports_a_server_only_hits_eml`), `clients/tui/src/commands.rs` (`the_eml_is_the_file_message_materialise_eml_wrote`); `clients/desktop/src/components/reader/reader.test.tsx` (`te exports the message as its .eml into the directory the Export dialog names, the Save dialog's last (PERSO-174)`, `the toolbar's Export .eml and Forward as attachment run te and cF on the open message (PERSO-174)`), `clients/desktop/src/keymap/keymap.test.tsx` (`te on a server-only hit exports the server's .eml through message_export's hit source, without a fetch (PERSO-174)`), `clients/desktop/src-tauri/src/attachments.rs` (`export_copies_the_messages_eml_and_releases_its_handle`, `a_server_only_hit_exports_through_its_message_id`)
- Status: routed (PERSO-174); GUI shipped (PERSO-174)
- Note: the bytes are the row's stored `raw` blob, which every IMAP ingest writes; a row without one, every Graph row, is answered from the server (`$value`, or `BODY.PEEK[]` by `Message-ID`), so the call waits as long as a server query.
  The file is named `<YYYY-MM-DD>_<subject>.eml` by `mp_core::parse::eml_name`, the subject cut by bytes on a char boundary as an attachment's name is.
  As in `ATT-02`, the destination is the client's to name and the result is a permanent user artifact; a draft has no message to export and says it is a file already, and a server-only hit is `message.materialise_eml`'s `hit: {message_id, mailbox}`, downloaded from the server and not ingested (the desktop's `message_export` with a hit `source`).

### ATT-03 Attach a file to a draft

- Classification: GUI parity
- Source anchor: TUI `ta` in the drafts mailbox (`clients/tui/src/app/keymap.rs:1342`), `resolve_attachment_paths` (`src/send.rs:1964`)
- Daemon surface: `draft.attach` with an absolute path
- GUI location: clients/desktop: `ta` in Drafts, the palette or the draft preview's Attach file open the "Attach file" dialog, whose File field takes an absolute or `~` path; `draft_attach` appends it through the daemon's `draft.attach`, and the preview's attachment list removes an entry with `draft_attachment_remove` over `draft.detach` (M3, #0131; reader.md, "Drafts and server-only hits")
- Validation: `tests/draft_integration.rs`; `clients/desktop/src/components/compose/compose.test.tsx` (`ta opens the path dialog, which keeps a missing file open and attaches an existing one`, `Browse attaches a file the native picker chose, under ~ as the draft keeps it, and a path outside home as picked`, `the preview lists the entries in order, opens one, flags a missing one and removes one`), `clients/desktop/src-tauri/src/attachments.rs` (`attach_appends_in_order_refuses_duplicates_and_missing_files_and_remove_rewrites`, `a_draft_entry_resolves_as_the_send_path_resolves_it`)
- Status: GUI shipped (M3, #0131) with a typed path, which Browse fills from the native file picker (`tauri-plugin-dialog`), under the home directory as `~/…`
- Note: appends to the `attachments:` frontmatter list and verifies the path at the prompt, so the GUI file picker applies the same verification.
  The daemon serves `draft.attach`, `draft.attachments` and `draft.detach` since #0131 ("Daemon gaps closed"), which the desktop calls, and the TUI's `ta` calls `draft.attach` since the daemon leftovers' TUI follow-up (2026-10-22) instead of rewriting the file itself.
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
- Source anchor: TUI `tb` (`clients/tui/src/app/keymap.rs:1126`), search overlay `b`
- Daemon surface: `message.materialise_html`, opened client-side through `parse::open_file_with_system`; on a Drafts row `draft.html`, the HTML part the send would build, which the TUI and the desktop write to a file and open the same way, and which the desktop's `mpmsg` scheme also serves to the reader frame (PERSO-136)
- GUI location: clients/desktop: `tb`, the reader toolbar's Open in browser and the palette hand the daemon's rendition to the default browser through `html_open`, a server-only hit's markup through `hit_html_open`, which writes it with the charset and the CSP into the app cache, and on a draft in Drafts the HTML the send would build through `draft_html_open`, `draft.html` written into the same cache as `renditions/draft-<id>/message.html`; the draft preview opens on the Markdown body in either reader mode, and `tt` toggles it with that HTML in the sandboxed reader frame on `mpmsg://localhost/<account>/drafts/<id>`, without touching the stored mode (M3, #0131, PERSO-136; reader.md, "The browser rendition", "Drafts and server-only hits")
- Validation: unit tests in `src/parse.rs` for the companion document; `the_browser_gets_the_html_blob_written_to_a_file`, `the_browser_rendition_inlines_cid_images_as_data_uris`, `tb_on_a_drafts_row_opens_the_html_the_send_would_build` and `tb_on_a_vanished_draft_says_the_preview_failed` (`src/tui_tests/actions_store.rs`); `tests/daemon_draft_html.rs` for `draft.html`; `clients/desktop/src/components/reader/reader.test.tsx` (`tb and the toolbar open the daemon's rendition in the browser, and a message without markup says so`, `opens the hit's own markup in the browser, and one with none has no button`), `clients/desktop/src-tauri/src/attachments.rs` (`html_opens_the_daemons_rendition_and_a_message_without_markup_is_none`, `a_hit_rendition_carries_the_charset_and_the_csp_and_opens`, `a_drafts_send_rendition_is_written_per_draft_and_opened`, `a_draft_id_names_one_directory_under_the_renditions`), `clients/desktop/src-tauri/src/reader.rs` (`a_draft_path_parses_into_an_account_and_an_id`, `a_draft_is_served_as_its_send_rendition_with_the_bridge`), `clients/desktop/src/app/attachments.test.ts` (`opens the HTML the send would build, through draft_html_open`, `a refused draft.html says why on the activity line and opens nothing`, `a draft that does not parse asks for the editor and calls nothing`), `clients/desktop/src/components/reader/readerMode.test.tsx` (`a draft opens on its Markdown in html mode, tt shows the send's HTML in the frame, tt again the Markdown, and nothing is stored`, `a save of the draft reloads the frame while it is shown`, `another draft and back opens on the Markdown again, and a message keeps the html default`), `clients/desktop/src/components/compose/compose.test.tsx` (`shows the headers, the status, the validation and the body of the selected draft`)
- Status: routed (P5-U6); GUI shipped (M3, #0131) for a received message and a server-only hit; a draft's rendition shipped in both clients (PERSO-136), in the desktop also as the draft preview's `tt` view
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
- Source anchor: `mp send-approved [-y] [--all-accounts]` (`src/main.rs`), TUI `cX` (`clients/tui/src/app/keymap.rs:1380`)
- Daemon surface: `send.approved`
- GUI location: clients/desktop: `cX` in Drafts and the palette ask "Send all approved emails?" with "In <mailbox label>", and the OK calls `send_approved` with `hold: true` for the selected account, awaited as the operation kind `send_approved`; the end says "Sent N, failed M" or "No approved emails found" (M3, #0131; shell.md, "Send")
- Validation: `tests/outbox_integration.rs`, `tests/daemon_send_slice.rs`; `clients/desktop/src/keymap/keymap.test.tsx` (`cX is Drafts only, and in Drafts sends every approved draft of the account`), `clients/desktop/src/app/reducer.test.ts` (`a batch says Sent N, failed M with its failures as an alert`), `clients/desktop/src-tauri/src/commands.rs` (`send_approved_is_awaited_as_send_approved`), `clients/desktop/src-tauri/src/fixture.rs` (`send_approved_sends_every_approved_draft_behind_one_hold`)
- Status: routed (P5-U6); GUI shipped (M3, #0131) for one account; the `--all-accounts` loop has no GUI path
- Note: `--all-accounts` is a loop in the client over `global_config.accounts` in configuration order, so `send.approved` names one account and a caller that sends `all_accounts` is refused.
  The TUI's `cX` went through it in P5-U6 and derives its line from the settled `{sent, failed}`; an account with neither is the "No approved emails found" the empty scan printed.
  The transport check stays client-side, because its sentence is the key's and the progress line has to say which transport was resolved.

### SND-03 Approve and send the current draft with one key

- Classification: GUI parity
- Source anchor: TUI `x` (`clients/tui/src/app/keymap.rs:706`)
- Daemon surface: `draft.validate`, `draft.approve`, then `send.draft`, in the TUI's `x` as in the desktop's `send_draft` (the TUI validated and approved the file itself until the daemon leftovers' TUI follow-up, 2026-10-22)
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
- Source anchor: TUI `ss` (`clients/tui/src/app/keymap.rs:794`) and `sS` (`clients/tui/src/app/keymap.rs:803`)
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
  `LoadMailbox` became `message.list` / `draft.list` in P5-U4, `message.list_stream` / `draft.list` since #0138, and the two fetch arms became `sync.quick` in P5-U6, each still on the worker thread it always had.
  P5-U8 took the wait off those threads: a pass is started by `commands::dispatch` and its finish arrives as an `operation.finished` event, so nothing polls `operation.status` any more.
  Whether an account has a server to auto-fetch from is `account.credentials`' answer, asked on the account's open thread before `mailbox.list` and carried in `BgResult::AccountOpened` (the daemon leftovers' TUI follow-up, 2026-10-22); the TUI loaded each account's IMAP, SMTP and Graph configuration itself before, password and token included.
  The `opening` -> ready transition is still `BgResult::AccountOpened` as well as the bootstrap's, because the store-backed open is what a client with a wedged session has left; the account runtime's readiness reaches the client as an `account.state_changed` beside it, which ends `opening` and recounts the account, the one path left after a resync bootstrap that caught the runtime opening.

### SYN-05 Sync health and error surfacing per account

- Classification: GUI parity
- Source anchor: `crates/mp-core/src/sync_health.rs`
- Daemon surface: `state.bootstrap` `sync_health` (`state`, `reason`, `at`), then `sync.completed` on `state.event`
- GUI location: clients/desktop: a health badge beside each account in the sidebar, which each `sync.completed` updates (M1, #0129)
- Validation: unit tests in `crates/mp-core/src/sync_health.rs`; `clients/tui/src/app/bootstrap.rs` (`a_failed_sync_health_in_the_snapshot_marks_the_account`); `clients/desktop/src/app/reducer.test.ts` (`tracks sync health and account state from events`)
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
- GUI location: clients/desktop: `Space c`, the sidebar's Contacts entry and the palette show the Contacts view, the selection account's ranked contacts from `contact_search` in a listbox named "Contacts"; `/` focuses its search field, which asks 150 ms after the typing pauses and drops the answer to a query typed over; beside the list, the cursor contact's details with its first and last seen, the TUI's detail pane (M4, #0131; shell.md, "Contacts")
- Validation: unit tests in `src/contacts/matcher.rs`, `tests/daemon_admin_slice.rs`; `clients/desktop/src/components/contacts/contacts.test.tsx` (`lists the selected account's ranked contacts in a listbox named Contacts, with their counts and scores`, `details the cursor contact as the TUI's pane does, first and last seen as dates`, `/ focuses the search field, typing asks once per pause, and Escape leaves it with the query kept`), `clients/desktop/src/app/contacts.test.ts` (`drops the answer of a query typed over, so the list is asked again for the new one`, `reads an account's list again when the view comes back to it with another query`), `clients/desktop/src-tauri/src/contacts.rs` (`rows_decode_with_the_recipient_quoted_where_the_name_needs_it`), `clients/desktop/src-tauri/src/fixture.rs` (`contact_search_matches_address_and_name_in_score_order_up_to_the_limit`)
- Status: routed (P4-U14); GUI shipped (M4, #0131) with at most 1000 rows per search, where the TUI lists its whole index
- Note: the TUI's Contacts view and its compose wizard's completion read the whole index through one `contact.search` with an empty query, off the UI thread, and rank a typed query over it with `mp_core::contacts::search`; the rows carry `first_seen` and `last_seen`, which the ranking and the detail pane read (the daemon leftovers' TUI follow-up, 2026-10-22, before which the TUI read the cache file).
  Validation: `clients/tui/src/app/keys.rs` (`the_contacts_view_shows_the_daemons_index_and_not_the_cache_file`, `a_refused_implicit_build_is_said_on_the_status_line`), `clients/tui/src/actions.rs` (`a_compose_wizard_completes_from_the_daemon_contact_index`).
  The desktop's details show the same two dates since the tidy-up of 2026-10-22, and both desktop fixtures' rows carry them.

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
- Source anchor: TUI `v` in the contacts view (`clients/tui/src/actions.rs`), `crates/mp-core/src/contacts/vcard.rs`, `contact.vcard_draft` in `src/daemon/methods/contact.rs`
- Daemon surface: `contact.vcard_draft` `{account, name, address, display_name}` -> `{draft, vcf}`: the daemon writes the "Contact: <name>" draft with no signature, the `.vcf` under the drafts directory's `_vcards/`, and its attachment (the daemon leftovers, 2026-10-22)
- GUI location: clients/desktop: `v`, the header's "Send vCard" and the palette's "Send contact as vCard" call `contact_vcard_draft`, one `contact.vcard_draft`, then the draft opens in the external editor (M4, #0131; shell.md, "Contacts", "Keys and actions", rust-layer.md, "The contacts")
- Validation: unit tests in `crates/mp-core/src/contacts/vcard.rs`, `tests/daemon_admin_slice.rs` (`contact_vcard_draft_writes_the_draft_and_attaches_the_vcard`); `clients/desktop/src/components/contacts/contacts.test.tsx` (`v writes a vCard draft to the contact and opens it in the editor`), `clients/desktop/src-tauri/src/contacts.rs` (`a_vcard_draft_writes_the_vcf_beside_the_drafts_and_attaches_it_once`, `a_vcard_draft_without_an_address_asks_nothing`)
- Status: routed in the TUI and the desktop (the daemon leftovers, 2026-10-22); GUI shipped (M4, #0131)

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
- Daemon surface: daemon-internal, announced as `contact.changed` `{account, contacts}` whenever an index is written: a rebuild, the build a search runs for an account with none, and the bump a sync or a send gives the addresses it saw (the daemon leftovers, 2026-10-22); a search whose own build the cache guard refused answers the kept index with `rebuild` set
- GUI location: clients/desktop: the Contacts view lists the daemon's ranking in its order, with each row's To, Cc and received counts and a query's match score; a `contact.changed` makes the account's list stale, read again when the view shows it, and a refused build of a search says the cache guard's line as a notice (M4, #0131; shell.md, "Contacts", "Staleness")
- Validation: unit tests in `src/contacts/rank.rs`, `src/contacts/extractor.rs`, `src/daemon/methods/contact.rs` (`a_refused_implicit_build_serves_the_kept_index_and_says_so`), `crates/mp-client/src/format.rs` (`a_refused_build_has_one_line_and_a_written_one_none`), `tests/daemon_admin_slice.rs` (`a_written_rebuild_publishes_contact_changed`); `clients/desktop/src/components/contacts/contacts.test.tsx` (`j, k, G and gg move the cursor, and a contact.changed while hidden reads the list on the next open`), `clients/desktop/src/app/contacts.test.ts` (`a written rebuild says how many, and its contact.changed reads the list again`, `a search whose own build the cache guard refused says so`)
- Status: routed (P4-U14); GUI shipped (M4, #0131); the index-changed event and the refused-build notice since the daemon leftovers (2026-10-22); `mp contacts search` says the notice on stderr, and the TUI, whose Contacts view reads the cache file, reads it again on `contact.changed`
- Note: implicit workflow that keeps the index current as mail arrives.

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
- Source anchor: TUI `sc` (`clients/tui/src/app/keymap.rs:821`)
- Daemon surface: `config.get`'s `path` for the location; the daemon reloads the file either way
- GUI location: clients/desktop: `sc` from every view, the palette's "Open config.toml in $EDITOR", and "Open config.toml" in the Settings view and on the config.toml banner call `config_open`, which opens `config.get`'s `path` in the external editor; the Settings view's Reload then calls `config_reload` (M4, #0131; shell.md, "config.toml and the daemon log" and "Settings")
- Validation: `clients/desktop/src/components/activity/activity.test.tsx` (`s c and s f open them from Mail and from Contacts, and s l works there too`, `says why when there is no config.toml or no log yet, and logs it as a warning`), `clients/desktop/src/components/settings/settings.test.tsx` (`Open config.toml hands the daemon's file to the editor`, `Reload says what the swap did, logs it once, and reads the configuration again`), `clients/desktop/src-tauri/src/daemon_files.rs` (`config_open_on_journals_the_editor_on_the_daemons_path`, `config_open_on_an_absent_configuration_is_not_found_and_opens_nothing`, `config_open_on_an_invalid_configuration_still_opens_it`)
- Status: GUI shipped (M4, #0131)
- Note: the GUI equivalent is the settings surface plus an explicit reveal or open action.

### INT-02 Open the log file in the editor

- Classification: GUI parity
- Source anchor: TUI `sf` (`clients/tui/src/app/keymap.rs:830`), the `OpenLogFile` action (`clients/tui/src/app/types.rs:1597`)
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
- Validation: `clients/desktop/src/components/reader/reader.test.tsx` (`shows a refused link in the reader footer and opens it only on the click, once`, `cannot hand a non-web scheme to the opener, and the notice dismisses`, `tb and the toolbar open the daemon's rendition in the browser, and a message without markup says so`), `clients/desktop/src/components/settings/wizard.test.tsx` (`a Microsoft 365 account signs its candidate in on the Connection step, tests it, and the write promotes the sign-in`)
- Status: GUI shipped (M1, #0129) for reader links, (M3, #0131) for HTML parts, and (M4, #0131) for the OAuth verification URL

### INT-05 Desktop notifications for new mail

- Classification: GUI parity
- Source anchor: `crates/mp-core/src/notify.rs`, using osascript on macOS and notify-send on Linux with sanitized payloads; a test build records into `notify::test_sink` instead and panics without a capture, so no test reaches the host's notification centre
- Daemon surface: the daemon decides a notification is warranted and emits `state.event`; the client holding the entitlement presents it
- GUI location: clients/desktop (M6, #0132)
- Validation: unit tests in `crates/mp-core/src/notify.rs`; `a_tick_with_arrivals_notifies_the_user_and_refreshes_the_list` (`src/tui_tests/events.rs`), `a_runtime_tick_reaches_a_subscribed_client_with_its_arrivals` (`tests/tui_daemon_recovery.rs`)
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
- Source anchor: TUI `!` (`clients/tui/src/app/keymap.rs:695`), `sl` (`clients/tui/src/app/keymap.rs:812`), and `/` inside the overlay (ACTIVITY LOG keymap section)
- Daemon surface: `state.event` activity stream; the overlay itself is client-side
- GUI location: clients/desktop: `sl`, the sidebar's Activity entry and the palette's "Activity log overlay" open the activity log dialog, the newest 100 lines this window reported (its notices, `sync.completed`, `config.changed`, `config.invalid` and the end of a hold) with their level, scrolled with `j`/`k`, `d`/`u`, `gg`/`G` and filtered with `/`; `!` hides or shows the activity area's notices and never a live hold card, since the desktop has no bottom log pane (M4, #0131; shell.md, "Activity log")
- Validation: TUI golden frames; `clients/desktop/src/components/activity/activity.test.tsx` (`s l lists what the window reported, oldest first, with each level, and Escape closes it`, `filters on text and level: / goes to the field, Enter back to the lines, Escape clears, then closes`, `opens at the end and scrolls with j/k, d/u, gg and G`, `hides the notices and never the live hold card, and shows them again`), `clients/desktop/src/app/activity.test.ts` (`keeps the newest ACTIVITY_LOG_CAP lines, oldest first, with rising ids`, `logs sync.completed at the daemon's severity, config.changed and config.invalid`)
- Status: GUI shipped (M4, #0131); the log holds only what this window heard since it started

### OBS-02 Command palette over every runnable action

- Classification: GUI parity
- Source anchor: TUI `:` (`clients/tui/src/app/keymap.rs:663`) and `Ctrl+p` (`clients/tui/src/app/keymap.rs:673`), `palette_actions()` (`clients/tui/src/app/keymap.rs:1812`)
- Daemon surface: client-side, derived from `KEYMAP` so it cannot drift from the bindings
- GUI location: clients/desktop: `:` or `Ctrl+p` opens the command palette over every row of the generated `keymap.json` and the desktop's own rows, each runnable or disabled with its badge (M1, #0129); a query matches each row's section, label and keys and lists the best match first, scored by cmdk's `command-score`, which the TUI's `command_score` ports
- Validation: unit tests in `clients/tui/src/app/keymap.rs`, `clients/tui/src/app/keys.rs` (`command_score_matches_cmdk`, `command_palette_ranks_the_best_match_first`), TUI golden frames; `clients/desktop/src/components/palette/palette.test.tsx` (`lists every action of the generated keymap`, `ranks the best match first across sections, so a typed name and Enter run it`, `marks what this build cannot run as disabled with its milestone`, `runs an enabled action`), `clients/desktop/src/keymap/keymap.test.tsx` (`: and Ctrl+p open the palette, ? the key help`)
- Status: GUI shipped (M1, #0129)

### OBS-03 Help overlay and hint bar

- Classification: GUI parity
- Source anchor: TUI `?` (`clients/tui/src/app/keymap.rs:652`), `help_sections()` (`clients/tui/src/app/keymap.rs:1758`)
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
- Note: every TUI theme except `terminal` paints its own background, and the TUI then draws a rounded outer frame in the theme's border colour as the edge between its ground and the terminal's (`clients/tui/src/ui/mod.rs`, `has_outer_frame`); the desktop has no counterpart, since its window has its own chrome.

### OBS-08 Quit the client while leaving durable work in place

- Classification: GUI parity
- Source anchor: TUI `q` (`clients/tui/src/app/keymap.rs:620`)
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
- GUI location: the desktop's health banner lines under the connection banner, one per check that is not `ok`, from the snapshot and `diagnostic.check_changed`, each also a line in the activity log (clients/desktop/docs/shell.md, "Health checks")
- Validation: `tests/daemon_diagnostics.rs`, `clients/tui/src/diagnostics_tests.rs` and `clients/desktop/src/app/health.test.ts` with `src/components/shell/health.test.tsx`; contract in [docs/tickets/0125-daemon-hardening.md](tickets/0125-daemon-hardening.md) (P6-U7)
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
Three moved or were imprecise in the plan's inventory and are corrected here.

- `ACC-11`: the `{{SIGNATURE}}` marker is not in `src/signatures.rs`; the splice and strip logic lives at `src/send.rs:185-268`, with the preview substitution at `clients/tui/src/ui/preview.rs:71`.
  `src/signatures.rs` holds the file and default lookup only.
- `MSG-08`: `queue_mark_open_read` is defined at `clients/tui/src/app/mod.rs:1230`; `clients/tui/src/app/keys.rs` calls it (lines 322 and 336), which is what the inventory's wording describes.
- `OBS-07`: there is no `[theme]` config section; `theme` is a top-level key (`src/config.rs:25`).
  `clients/tui/src/theme.rs` resolves.

The `clients/tui/src/app/keymap.rs` anchors were resolved again on 2026-10-22, each at the line that holds its key string.
`ACC-10`'s signature files are `crates/mp-core/src/signatures.rs`, resolved against the tree of M4 (#0131).

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
