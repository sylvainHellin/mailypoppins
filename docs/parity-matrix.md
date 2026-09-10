# Feature-parity matrix

Every capability the current CLI and TUI deliver, one entry per stable identifier, carried from the verified capability inventory of the native-GUI/daemon plan (`.agents/workflow/native-gui-daemon/plan.md`, phase 0, ticket #0118).
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

- GUI parity: a user-facing capability that Phase 9 must deliver in the GUI.
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

Phase 0 does not design the GUI, so every entry reads `TBD (Phase 9)`.

### Validation

Each entry names the coverage that exists today.
On top of that, and not repeated per entry: every daemon-served capability gains a protocol contract test in Phase 2 or later, and every GUI-parity capability gains a GUI and an end-to-end check in Phase 9.

### Status vocabulary

- `not started`: inventoried here, no daemon or GUI work done.
- `routed (<unit>)`: the CLI surface answers from the daemon, byte-identically to the pre-daemon binary, with the unit that moved it named.
- `retired`: the capability was removed from the product; the identifier stays reserved.
- `deferred`: parity is agreed but scheduled out of the first GUI release, with the deferral recorded in the backlog.

## Accounts, configuration, secrets, and signatures

### ACC-01 Interactive account setup wizard

- Classification: GUI parity
- Source anchor: `mp config init`, `src/main.rs`, `src/config_cmd/init.rs`
- Daemon surface: `config.get` before the first prompt, then `config.reload` once the wizard has written; `config.init`, `config.set_password`, `operation.*` for the multi-step pass, `state.event` once the account exists
- GUI location: TBD (Phase 9)
- Validation: `tests/daemon_admin_slice.rs` (`mp_config_init_prompts_in_the_client`), otherwise manual
- Status: routed (P4-U14)
- Note: the wizard writes `config.toml` and one secret in a single pass, so the GUI drives it through `config.*` rather than spawning the CLI.
  P4-U14 routed the branch a parity test can reach - the overwrite question, which the client asks after `config.get` has told it whether a configuration exists and where - and left the wizard's own writes in the client, which reload the daemon when they finish; the pass past the first prompt dials a mail server and is pinned by nothing.

### ACC-02 Add a further account to an existing configuration

- Classification: GUI parity
- Source anchor: `mp config add-account`, `src/config_cmd/init.rs`
- Daemon surface: `config.get` before the first prompt, then `config.reload`; `config.add_account`, `state.event`
- GUI location: TBD (Phase 9)
- Validation: `tests/daemon_admin_slice.rs` (`mp_config_add_account_refuses_without_a_configuration`), otherwise manual
- Status: routed (P4-U14)
- Note: the refusal when there is no configuration to add to is the daemon's answer, rendered here; the wizard past it is `ACC-01`'s note.

### ACC-03 Show the effective configuration

- Classification: diagnostics and maintenance
- Source anchor: `mp config show`, `src/config_cmd/show.rs`
- Daemon surface: `config.get`
- GUI location: TBD (Phase 9)
- Validation: `tests/daemon_admin_slice.rs` (`mp_config_show_matches_the_oracle`)
- Status: routed (P4-U14)
- Note: the output is redacted, and after the cutover the daemon is the only reader of the underlying file.
  One divergence from the `pre-daemon` binary is deliberate and older than this slice: `config.get` reports the effective configuration, which `docs/daemon-protocol.md` defines as the document *after serde defaults*, so `mp config show` prints `smtp.port = 465` and `imap.port = 993` for a configuration that omits them where the oracle printed `0`.
  The `ACC-03` parity row names every port in its fixture, which takes the difference out of the comparison and leaves the row measuring what it is about.

### ACC-04 Print the configuration file path

- Classification: diagnostics and maintenance
- Source anchor: `mp config path`, `src/config_cmd/mod.rs`
- Daemon surface: client-side; the path is computed, so the command must keep running with no daemon
- GUI location: TBD (Phase 9)
- Validation: `tests/daemon_admin_slice.rs` (`mp_config_path_never_contacts_a_daemon`)
- Status: client-side, confirmed (P4-U14)
- Note: on `needs_daemon`'s no-daemon list for good, and from P4-U14 the `UNMIGRATED` control row of `tests/daemon_parity_harness.rs`: after the admin slice it is the only command in the product a daemon-era binary answers in process.

### ACC-05 Store an SMTP or IMAP password in the active secrets backend

- Classification: GUI parity
- Source anchor: `mp config set-password <smtp|imap> [--account]`, `src/main.rs`, `src/config_cmd/password.rs`
- Daemon surface: `config.set_password`
- GUI location: TBD (Phase 9)
- Validation: `tests/secrets_integration.rs` for the backend, `tests/daemon_admin_slice.rs` (`mp_config_set_password_reads_the_password_in_the_client`)
- Status: routed (P4-U14)
- Note: secret values travel only on the local socket and never appear in logs, diagnostics, or protocol errors.

### ACC-06 OAuth2 device-code login

- Classification: GUI parity
- Source anchor: `mp config oauth2-login [--account]`, `src/config_cmd/oauth2.rs`, `src/oauth2.rs`
- Daemon surface: `config.oauth2_login` as an `operation.*` with the user code and verification URL on `state.event`
- GUI location: TBD (Phase 9)
- Validation: `tests/daemon_admin_slice.rs` pins the three refusals and the rendering; the flow itself is manual and requires a live provider
- Status: routed (P4-U14)
- Note: the client renders the code and opens the browser as a client-side integration (`INT-04`).
  The progress payload is `{phase: "device_code", done: 0, total: null, message: "<verification_uri> <user_code>"}`, and `mp_client::format::{oauth2_start_line, oauth2_device_code_lines, oauth2_stored_line}` are the three lines a GUI reproduces.
  The IMAP, SMTP and Graph connection tests the command ran after acquiring a token are not on the routed path: they need the token the daemon now holds, and they belong to the account slice.

### ACC-07 Reset secrets, wiping the encrypted file and the OAuth token caches

- Classification: diagnostics and maintenance
- Source anchor: `mp config reset-secrets`, `src/config_cmd/reset.rs`
- Daemon surface: `config.reset_secrets`, then one `config.set_password` per re-entered credential
- GUI location: TBD (Phase 9)
- Validation: `tests/daemon_admin_slice.rs` (declined and confirmed)
- Status: routed (P4-U14)
- Note: the recovery path after a restore onto a new machine, where the machine-uid derived key no longer decrypts the file.
  The confirmation and every password prompt stay in the client; the answer names the secrets file first and then the token caches, in path order, where the pre-daemon binary walked `read_dir` unsorted.

### ACC-08 Secrets backend, keyring plus a ChaCha20-Poly1305 file keyed through HKDF from the machine uid

- Classification: daemon administration
- Source anchor: `src/secrets.rs`, `tests/secrets_integration.rs`
- Daemon surface: daemon-internal; only the daemon opens the backend after the cutover
- GUI location: TBD (Phase 9)
- Validation: `tests/secrets_integration.rs`
- Status: not started

### ACC-09 Account and signature selection on every command

- Classification: GUI parity
- Source anchor: the global arguments `-A/--account`, `-s/--signature`, `--no-signature` in `src/main.rs` (`no_signature`, `src/main.rs:41`), `src/signatures.rs`
- Daemon surface: an account parameter on every domain method, `signature.list`, `state.bootstrap`
- GUI location: TBD (Phase 9)
- Validation: `tests/cli_help_snapshot.rs` pins the global arguments
- Status: not started
- Note: the GUI equivalent is the active-account selector plus a per-composition signature choice.

### ACC-10 Signature file management

- Classification: GUI parity
- Source anchor: the TUI `cs` overlay (`src/tui/app/keymap.rs:588`), `src/signatures.rs`, the per-account default recorded in `state.json`
- Daemon surface: `signature.list`, `signature.read`, `signature.write`, `signature.create`, `signature.rename`, `signature.delete`, `signature.set_default`
- GUI location: TBD (Phase 9)
- Validation: manual
- Status: not started
- Note: there is no CLI equivalent, so the GUI takes this capability from the TUI, and inline-text signatures are edited through a temporary copy.

### ACC-11 Signature injection into a draft

- Classification: GUI parity
- Source anchor: the `{{SIGNATURE}}` marker handling in `src/send.rs:185-268`, the file and default lookup in `src/signatures.rs`
- Daemon surface: `draft.create`, `draft.reply`, `draft.forward`, and `draft.set_recipients` all re-splice
- GUI location: TBD (Phase 9)
- Validation: `tests/draft_integration.rs`, plus the marker assertions in `src/tui/actions.rs` unit tests
- Status: not started
- Note: editing recipients re-splices the block, which is what makes this its own capability.

### ACC-12 Multi-account operation

- Classification: GUI parity
- Source anchor: the per-command account resolution and `--all-accounts` in `src/main.rs`
- Daemon surface: `account.list`, `state.bootstrap` per account, `state.event`
- GUI location: TBD (Phase 9)
- Validation: manual
- Status: not started
- Note: account switching preserves per-account list and selection state.

## Mailbox navigation and view switching

### MBX-01 List server mailboxes

- Classification: diagnostics and maintenance
- Source anchor: `mp list-mailboxes`, `src/main.rs`, `src/imap_client/mod.rs`
- Daemon surface: `mailbox.list_server`
- GUI location: TBD (Phase 9)
- Validation: manual, requires a live server; refusals and routing in `tests/daemon_sync_slice.rs`
- Status: routed (P4-U10); GUI not started
- Note: a live server call, distinct from the mailbox hierarchy the store already holds. The result carries `source` (`imap` or `graph`) because the two transports report different things about a mailbox: Graph's folder list has the item counts this listing prints and IMAP's `LIST` has the attributes and the delimiter instead.

### MBX-02 Browse and select mailboxes in the sidebar

- Classification: GUI parity
- Source anchor: TUI `j/k` and `Enter` (`src/tui/app/keymap.rs:636`), `gm` (`src/tui/app/keymap.rs:590`), `src/tui/ui/sidebar.rs`
- Daemon surface: `state.bootstrap` mailbox summaries, `message.list` on selection
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames under `src/tui/`
- Status: not started

### MBX-03 Jump to a mailbox by digit 1 through 9

- Classification: GUI parity
- Source anchor: `src/tui/app/keymap.rs:558`
- Daemon surface: client-side over the bootstrap mailbox list, then `message.list`
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started

### MBX-04 Switch account

- Classification: GUI parity
- Source anchor: TUI `ga` (`src/tui/app/keymap.rs:591`), guarded by `Guard::MultiAccount` so it appears only with more than one configured account
- Daemon surface: `account.list`, a second `state.bootstrap`
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started

### MBX-05 Switch between the Mail, Contacts, and Calendar views

- Classification: GUI parity
- Source anchor: TUI `Space m`, `Space c`, `Space a` (`src/tui/app/keymap.rs:601-603`)
- Daemon surface: client-side; the view switch reads data already bootstrapped
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started

### MBX-06 Cycle pane focus and zoom the focused pane

- Classification: GUI parity
- Source anchor: TUI `Tab` (`src/tui/app/keymap.rs:559`), `Shift+Tab` (`src/tui/app/keymap.rs:560`), `z` (`src/tui/app/keymap.rs:569`)
- Daemon surface: client-side
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started
- Note: purely presentation state, so it never enters the canonical snapshot.

### MBX-07 Mailbox roles, slugs, sidebar labels, unread counts, and outbox badges

- Classification: GUI parity
- Source anchor: `src/tui/ui/sidebar.rs` over the store's mailbox rows
- Daemon surface: `state.bootstrap`, then `state.event` for count changes
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started
- Note: delivered through the bootstrap snapshot rather than a query.

## Listing, filtering, and search

### LST-01 List received messages offline, grouped by mailbox

- Classification: GUI parity
- Source anchor: `mp list-messages [--mailbox] [-n]`, `src/main.rs`, `list_mailbox` (`src/store/read.rs:178`)
- Daemon surface: `message.list`, one call per listed mailbox
- GUI location: TBD (Phase 9)
- Validation: `tests/cli_read_surface_integration.rs`, `tests/daemon_read_slice.rs`
- Status: routed (P4-U4); GUI not started
- Note: the mailbox argument accepts a role, a slug, or the sidebar label, and the default lists every mailbox of the account. The name is resolved client-side against the configuration, so an unknown one is refused without a round trip and in the words it has always been refused in.

### LST-02 Navigate a list with per-item movement, top and bottom jumps, and half-page scrolling

- Classification: GUI parity
- Source anchor: TUI `j/k`, `gg/G`, `Ctrl+d`, `Ctrl+u` in the EMAIL LIST and BODY keymap sections (`src/tui/app/keymap.rs`)
- Daemon surface: client-side over the list `message.list` returned
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started

### LST-03 Jump to a date in the list

- Classification: GUI parity
- Source anchor: TUI `gt` (`src/tui/app/keymap.rs:652`), `src/tui/app/jump_date.rs`
- Daemon surface: client-side while the list is whole; `message.jump_to_date` if paging lands
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/tui/app/jump_date.rs`
- Status: not started
- Note: accepts relative expressions such as "last week" alongside absolute dates.

### LST-04 Filter the current list by metadata

- Classification: GUI parity
- Source anchor: TUI `fm` (`src/tui/app/keymap.rs:624`)
- Daemon surface: client-side while the list is whole; `message.filter` if paging lands
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started

### LST-05 Toggle a flagged-only filter

- Classification: GUI parity
- Source anchor: TUI `fF` (`src/tui/app/keymap.rs:673`)
- Daemon surface: client-side while the list is whole; `message.filter` if paging lands
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started

### LST-06 Search with one query grammar across every backend

- Classification: GUI parity
- Source anchor: `mp search [query] [--mailbox] [field flags] [-n] [--full]`, `SEARCH_LONG_ABOUT` (`src/main.rs:49`), `src/search.rs`, `src/imap_client/search.rs`
- Daemon surface: `message.search` as an `operation.*` for the server leg
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/search.rs`, `tests/cli_help_snapshot.rs` for the grammar's help text
- Status: not started
- Note: the grammar covers `from:`, `to:`, `cc:`, `subject:`, `body:`, `filename:`, `has:attachment`, `before:`, `after:` with `since:` as an alias, quoted phrases, `OR` groups, `in:`, and `message-id:`; `filename:` resolves only on Gmail, Exchange, or the local index.

### LST-07 Search the local ranked full-text index across every synced mailbox

- Classification: GUI parity
- Source anchor: `mp search --local`, `src/store/search.rs`, `tests/store_search_integration.rs`
- Daemon surface: `message.search`, whose params mirror the command's flags and whose hits come back in the store's ranking order
- GUI location: TBD (Phase 9)
- Validation: `tests/store_search_integration.rs`, `tests/daemon_read_slice.rs`
- Status: routed (P4-U4); GUI not started
- Note: `--body` is `body_query` on the wire, because `body` is already the `--full` switch; the client sends what the user typed and the daemon builds the query with `search::from_cli`, so one parser still serves every backend.

### LST-08 Merged search in the TUI

- Classification: GUI parity
- Source anchor: TUI `ff` (`src/tui/app/keymap.rs:581`), `Action::ServerSearch` (`src/tui/app/types.rs:1619`)
- Daemon surface: `message.search` local first, then the server leg as an `operation.*` streaming results on `state.event`
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started
- Note: deduplication is by Message-ID, so a message found twice appears once.

### LST-09 Act on a search result without leaving the overlay

- Classification: GUI parity
- Source anchor: the SERVER SEARCH keymap section (`src/tui/app/keymap.rs`): `Enter`, `e`, `y`, `f`, `r`, `R`, `w`, `a`, `b`, `o`, `O`
- Daemon surface: `message.get`, `message.materialize`, `message.fetch`, `message.archive`, `draft.reply`, `draft.forward`, `message.save_attachments`
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started

### LST-10 Show the conversation a message belongs to

- Classification: GUI parity
- Source anchor: TUI `tt` (`src/tui/app/keymap.rs:630`), the thread overlay in `src/tui/ui/overlays.rs`
- Daemon surface: `message.thread`
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started

### LST-11 Legacy server fetch with inline filters

- Classification: diagnostics and maintenance
- Source anchor: `mp fetch [--from --to --cc --subject --body --since --before -n --full --mailbox]`, `src/main.rs`
- Daemon surface: `sync.fetch_legacy`
- GUI location: TBD (Phase 9)
- Validation: `tests/cli_help_snapshot.rs`
- Status: not started
- Note: superseded by `sync` plus `search`, kept because the migration preserves command surfaces; the deprecation decision is deferred to `BACKLOG.md` (`ANO-3`).

### LST-12 Dump message envelopes as NDJSON

- Classification: CLI automation
- Source anchor: `mp dump-mailbox --json [--mailbox ...]`, `src/dump.rs`, `docs/dump-allow-list.md`, `tests/dump_mailbox_integration.rs`
- Daemon surface: `message.list` with `projection: "envelope"`, whose output must stay byte-identical to the direct read
- GUI location: TBD (Phase 9)
- Validation: `tests/dump_mailbox_integration.rs`, `tests/daemon_read_slice.rs`
- Status: routed (P4-U4); GUI not started
- Note: three contracts survive the move to an RPC data source: two runs over an unchanged store are byte-identical, `--json` stays required, and no filesystem path appears in the output. P4-U4 shipped the dump as a projection of `message.list` rather than the `message.dump` this row first proposed: the records answer the same question a listing does, one answer per account covers every selected mailbox in the dump's own sort order, and the client re-serialises `dump::EnvelopeRecord` with `dump::to_ndjson`, so the ordering contract and the field order stay in the module that owns them.

### LST-13 Coalesce queued input before repainting

- Classification: GUI parity
- Source anchor: `MAX_COALESCED_EVENTS` (`src/tui/mod.rs:47`), `COALESCE_BUDGET` (`src/tui/mod.rs:55`) and the drain in `src/tui/mod.rs`, `poll_pending_event` (`src/tui/event.rs:32`), `Action::suspends_terminal` (`src/tui/app/types.rs:1716`)
- Daemon surface: client-side; the obligation is that a held key does not produce one round trip per repeat
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/tui/mod.rs`
- Status: not started
- Note: event order is preserved, so leader keys and resizes are unaffected; the drain stops when the app is no longer running or an action hands the terminal to `$EDITOR`, whose GUI counterpart is the handoff into the Neovim PTY.

## Reading and rendering

### RD-01 Read one received message from the local store while offline

- Classification: GUI parity
- Source anchor: `mp show <selector> [--mailbox]`, `src/read_cmd.rs`, `tests/cli_read_surface_integration.rs`
- Daemon surface: `message.get`, addressed by `"<mailbox>/<uid>"` or by the selector the daemon resolves
- GUI location: TBD (Phase 9)
- Validation: `tests/cli_read_surface_integration.rs`, `tests/daemon_read_slice.rs`
- Status: routed (P4-U4); GUI not started
- Note: the selector crosses the socket unresolved, because resolving one needs the store the client no longer has; which account it names stays a client-side decision.

### RD-02 Emit one message as a single JSON object with headers, attachments, and body

- Classification: CLI automation
- Source anchor: `mp show --json`, `src/main.rs`, `src/read_cmd.rs`
- Daemon surface: `message.get`, whose result *is* this record
- GUI location: TBD (Phase 9)
- Validation: `tests/cli_read_surface_integration.rs`, `tests/daemon_read_slice.rs`
- Status: routed (P4-U4); GUI not started
- Note: there is no second JSON projection. `message.get` returns `read_cmd::ShownMessage` field for field and `--json` prints it re-serialised, so the machine-facing answer cannot drift from the one the text layout renders.

### RD-03 Read HTML-dominant mail as the plain text flattened out of the markup

- Classification: GUI parity
- Source anchor: `wrap_and_style_body` (`src/tui/ui/preview.rs:522`) over the body `parse::html_to_plain` (`src/parse.rs:199`) produced at ingest
- Daemon surface: `message.get` returns the flattened body
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/tui/ui/preview.rs`, `src/parse.rs`, `tests/daemon_read_slice.rs`
- Status: routed for `mp show` (P4-U4); GUI not started
- Note: #0111 retired the html2text rich render #0091 had added, so links, emphasis, tables, and lists arrive as a wrapped block and `b` / `tb` is the styled view; a richer GUI rendering stays derived content rather than an embedded raw remote HTML document.

### RD-04 Headers pane with Bcc, Reply-To, the attachment marker, and clamped scrolling

- Classification: GUI parity
- Source anchor: `src/tui/ui/headers.rs`, the HEADERS keymap section
- Daemon surface: `message.get` header block
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/tui/ui/headers.rs`, TUI golden frames
- Status: not started

### RD-05 Inline image rendering in the reader

- Classification: GUI parity
- Source anchor: none; `src/tui/images.rs` was deleted with the `ratatui-image` and `image` dependencies and the startup graphics-capability probe
- Daemon surface: none
- GUI location: none
- Validation: none
- Status: retired by #0109, no parity obligation
- Note: `parse::inline_images` (`src/parse.rs:1078`) and `parse::embed_inline_images` (`src/parse.rs:1037`) remain, feeding the browser view and the `.html` companion, which is where the images are seen at full size.

### RD-06 Open a message read-only in `$EDITOR` as a Markdown rendition

- Classification: GUI parity
- Source anchor: TUI `Enter / e` (`src/tui/app/keymap.rs:611`), search overlay `e`
- Daemon surface: `message.materialize` returning a rendition handle the client opens
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started
- Note: the handle keeps its blob alive until release or expiry (`ANO-6`).

### RD-07 Copy a message's `mp://` selector to the clipboard

- Classification: GUI parity
- Source anchor: TUI `y` (`src/tui/app/keymap.rs:618`)
- Daemon surface: client-side over the selector already in the snapshot
- GUI location: TBD (Phase 9)
- Validation: `tests/cli_selector_contract.rs` for the selector shape
- Status: not started

### RD-08 Copy the Markdown rendition path of a search hit

- Classification: GUI parity
- Source anchor: the search overlay `y`, the action set in `src/tui/app/types.rs`
- Daemon surface: `message.materialize`, then a client-side clipboard write
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started

## Selection and message actions

### MSG-01 Archive a received message on the server and locally

- Classification: GUI parity
- Source anchor: `mp archive <selector> [--mailbox]` (`src/main.rs`), TUI `a` (`src/tui/app/keymap.rs:613`)
- Daemon surface: `message.archive`, addressed by `"<mailbox>/<uid>"` or by the selector the daemon resolves; the daemon commits the row move and drains the owed server op before it answers
- GUI location: TBD (Phase 9)
- Validation: `tests/cli_selector_contract.rs`, `tests/daemon_mutation_slice.rs`, TUI golden frames
- Status: routed (P4-U8); GUI not started
- Note: over an account with no credentials the backend refuses before the store is touched, which is the half the fixture reaches; the successful drain and its rollback wait on a fake IMAP backend.

### MSG-02 Delete a received message or a local draft

- Classification: GUI parity
- Source anchor: `mp delete <selector> [--mailbox] [--force]`, `mp delete --sent` (`src/main.rs`), TUI `d`
- Daemon surface: `message.delete` for received mail, `draft.discard` for a draft and for the `--sent` sweep, which is a parameter of the same method rather than one of its own
- GUI location: TBD (Phase 9)
- Validation: `tests/draft_integration.rs`, `tests/daemon_mutation_slice.rs`
- Status: routed (P4-U8); GUI not started
- Note: `--force` is required to delete an approved draft because that is a queued send, and `--sent` clears every sent draft of the account and takes no selector.
- Note: `--force` is required to delete an approved draft because that is a queued send, and `--sent` clears every sent draft of the account and takes no selector.

### MSG-03 Toggle read and unread

- Classification: GUI parity
- Source anchor: TUI `u` (`src/tui/app/keymap.rs:615`)
- Daemon surface: `message.set_read`
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started

### MSG-04 Toggle the `\Flagged` star

- Classification: GUI parity
- Source anchor: TUI `*` (`src/tui/app/keymap.rs:616`)
- Daemon surface: `message.set_flag`
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started
- Note: on a batch, flagging wins whenever any selected message is unflagged.

### MSG-05 Move a message to another mailbox through a fuzzy picker

- Classification: GUI parity
- Source anchor: TUI `M`, `Action::MoveToMailbox` (`src/tui/app/types.rs:1571`)
- Daemon surface: `message.move`, plus the mailbox list from `state.bootstrap`
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started

### MSG-06 Multi-select and batch actions

- Classification: GUI parity
- Source anchor: TUI `v` (`src/tui/app/keymap.rs:653`), `Ctrl+a` (`src/tui/app/keymap.rs:654`), the batch actions in `src/tui/app/types.rs`
- Daemon surface: batch forms of `message.set_read`, `message.set_flag`, `message.archive`, `message.delete`, `draft.delete`, `draft.approve`, `draft.demote`; selection stays client-side
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started

### MSG-07 Confirmation dialogs guarding destructive actions

- Classification: GUI parity
- Source anchor: the confirm variants in `src/tui/app/types.rs`, covering approve, demote, archive, delete, send, send-approved, and signature deletion
- Daemon surface: client-side, over the same methods
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started

### MSG-08 Mark a message read on an explicit open

- Classification: GUI parity
- Source anchor: `mark_open_read` (`src/tui/actions.rs:3339`), reached from the received-row branch of `Action::EditCurrent` and from `Action::MarkAsRead`, which `queue_mark_open_read` (defined at `src/tui/app/mod.rs:1179`, pushed from `src/tui/app/keys.rs:305`) queues on a focus move into the body pane
- Daemon surface: `message.set_read` carrying the opened `MessageRef`
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/tui/actions.rs`
- Status: not started
- Note: #0110 retired the #0087 trigger that fired on every cursor move, so walking the list marks nothing and the GUI marks on the open rather than on selection; the action carries the `MessageRef` the open resolved, so a coalesced key batch marks the row that was opened.

### MSG-09 Optimistic local mutation state reconciled against the server

- Classification: GUI parity
- Source anchor: `src/pending_ops.rs`, `src/ops.rs`
- Daemon surface: `state.event` carrying pending and reconciled states
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/pending_ops.rs`, `src/ops.rs`
- Status: not started

## Drafts, composition, reply, and forward

### DFT-01 Create a draft from the template and print its selector

- Classification: GUI parity
- Source anchor: `mp new <name>` (`src/main.rs`), TUI `cn` (`src/tui/app/keymap.rs:583`)
- Daemon surface: `draft.create`
- GUI location: TBD (Phase 9)
- Validation: `tests/draft_integration.rs`, `tests/daemon_draft_slice.rs`
- Status: routed (P4-U6); GUI not started

### DFT-02 List the account's drafts, optionally filtered by status

- Classification: GUI parity
- Source anchor: `mp list [--status]`, `src/main.rs`
- Daemon surface: `draft.list`
- GUI location: TBD (Phase 9)
- Validation: `tests/draft_integration.rs`, `tests/daemon_draft_slice.rs`
- Status: routed (P4-U6); GUI not started

### DFT-03 Validate draft frontmatter

- Classification: GUI parity
- Source anchor: `mp validate [selector]`, `src/draft.rs`
- Daemon surface: `draft.validate`
- GUI location: TBD (Phase 9)
- Validation: `tests/draft_integration.rs`, `tests/daemon_draft_slice.rs`, unit tests in `src/draft.rs`
- Status: routed (P4-U6); GUI not started
- Note: an invalid draft stays editable and cannot be approved or sent.

### DFT-04 Approve a draft and demote it back to draft status

- Classification: GUI parity
- Source anchor: `mp mark-approved`, `mp mark-draft` (`src/main.rs`), TUI `cA` and `cD` (`src/tui/app/keymap.rs:667-668`)
- Daemon surface: `draft.approve`, `draft.demote`, resolved through `draft.path` first so the client knows the previous status
- GUI location: TBD (Phase 9)
- Validation: `tests/draft_integration.rs`, `tests/daemon_draft_slice.rs`
- Status: routed (P4-U6); GUI not started

### DFT-05 Preview a draft as a dry run through a bare selector

- Classification: GUI parity
- Source anchor: the top-level positional `[SELECTOR]` argument in `src/main.rs`
- Daemon surface: `draft.preview`
- GUI location: TBD (Phase 9)
- Validation: `tests/cli_selector_contract.rs`, `tests/mime_oracle_integration.rs`, `tests/daemon_draft_slice.rs`
- Status: routed (P4-U6); GUI not started

### DFT-06 Resolve a draft selector to its filesystem path

- Classification: CLI automation
- Source anchor: `mp path <selector>`, `src/main.rs`, `src/selector.rs`
- Daemon surface: `draft.path`
- GUI location: TBD (Phase 9)
- Validation: `tests/cli_selector_contract.rs`, `tests/daemon_draft_slice.rs`
- Status: routed (P4-U6); GUI not started
- Note: the only selector-to-path edge, and the handle external editors and agents use, so it stays supported under the filesystem boundary.

### DFT-07 Edit a draft in the editor

- Classification: GUI parity
- Source anchor: `mp edit <selector>`, `src/main.rs`
- Daemon surface: `draft.path`, then a client-side editor session on the canonical file
- GUI location: TBD (Phase 9)
- Validation: `tests/daemon_draft_slice.rs`, with a stub editor that records the path it was handed
- Status: routed (P4-U6); GUI not started
- Note: the GUI equivalent is the embedded Neovim session on the same file.

### DFT-08 Create a reply or a reply-all draft from a received message

- Classification: GUI parity
- Source anchor: `mp reply <selector> [--all] [--mailbox]` (`src/main.rs`), TUI `r`, `cr` (`src/tui/app/keymap.rs:626`), `ca`, search overlay `r` and `R`
- Daemon surface: `draft.reply`
- GUI location: TBD (Phase 9)
- Validation: `tests/draft_integration.rs`, `tests/daemon_draft_slice.rs`
- Status: routed (P4-U6); GUI not started

### DFT-09 Forward a message to new recipients

- Classification: GUI parity
- Source anchor: `mp forward <selector> [--mailbox]` (`src/main.rs`), TUI `cf`, search overlay `w`
- Daemon surface: `draft.forward`
- GUI location: TBD (Phase 9)
- Validation: `tests/draft_integration.rs`, `tests/mime_oracle_integration.rs`, `tests/daemon_draft_slice.rs`
- Status: routed (P4-U6); GUI not started
- Note: the forward carries the original attachments, which the GUI must reproduce rather than dropping.

### DFT-10 Compose wizard for new and forwarded mail

- Classification: GUI parity
- Source anchor: the compose wizard variants in `src/tui/app/types.rs`, `src/tui/ui/compose.rs`
- Daemon surface: `draft.create`, `signature.list`; the wizard itself is client-side
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started
- Note: an inline body field, a signature picker, and a submit chord; the overlay-internal keys go into `docs/baselines/pre-daemon/manual-keys.md`, the P0-U2 inventory (`ANO-2`).

### DFT-11 Edit the recipients of an existing draft

- Classification: GUI parity
- Source anchor: TUI `ce` in the drafts mailbox (`src/tui/app/keymap.rs:656`)
- Daemon surface: `draft.set_recipients`, which re-splices the signature block
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started

### DFT-12 Watch draft files and refresh the derived index after an external edit

- Classification: GUI parity
- Source anchor: implicit workflow, no command; the drafts index refresh in `src/tui/`
- Daemon surface: daemon-owned watcher emitting `state.event`
- GUI location: TBD (Phase 9)
- Validation: manual
- Status: not started
- Note: the mechanism that keeps the GUI correct while Neovim writes the file.

## Attachments

### ATT-01 Open a received message's attachment in the default application

- Classification: GUI parity
- Source anchor: `mp open <selector> [--mailbox]` (`src/main.rs`), TUI `to`, search overlay `o`
- Daemon surface: `message.materialise_attachment`, one call per part, opened client-side through `parse::open_file_with_system`
- GUI location: TBD (Phase 9)
- Validation: `tests/cli_selector_contract.rs`, `tests/daemon_mutation_slice.rs`
- Status: routed (P4-U8); GUI not started
- Note: the printed path is the one row of the slice that is not byte-identical to the pre-daemon binary and cannot be: a materialised file lives under `<data_dir>/runtime/handles/<handle>/` with a lifetime attached, rather than in the client's own temp directory. The handle is deliberately not released, because the viewer just launched is holding the file.

### ATT-02 Save a received message's attachments into a client-named directory

- Classification: GUI parity
- Source anchor: `mp save <selector> [-o dir] [--mailbox]`, the option at `src/main.rs:301` and the handler in `src/main.rs`
- Daemon surface: `message.materialise_attachment`, one call per part, plus a client-side copy into the destination and a `message.release_handle` per part
- GUI location: TBD (Phase 9)
- Validation: `tests/cli_selector_contract.rs`, `tests/daemon_mutation_slice.rs`
- Status: routed (P4-U8); GUI not started
- Note: the destination defaults to the current directory and only the client knows what that means (`ANO-15`), so the client resolves it twice over: the absolute form anchors the writes, and the spelling the user typed is what the `✓` lines print. The result is a permanent user artifact rather than a daemon-owned handle with a lifetime, which is what separates this entry from `ATT-01`, `ATT-04`, and `ATT-05`. The daemon never renames a part, so the `_1` rule for two parts sharing a name is applied client-side, within one call.

### ATT-03 Attach a file to a draft

- Classification: GUI parity
- Source anchor: TUI `ta` in the drafts mailbox (`src/tui/app/keymap.rs:659`), `resolve_attachment_paths` (`src/send.rs:1902`)
- Daemon surface: `draft.attach` with an absolute path
- GUI location: TBD (Phase 9)
- Validation: `tests/draft_integration.rs`
- Status: not started
- Note: appends to the `attachments:` frontmatter list and verifies the path at the prompt, so the GUI file picker applies the same verification; a relative entry resolves against the process working directory, so the same absolutisation rule applies at the prompt and a hand-written relative entry keeps resolving where its author expects.

### ATT-04 Open a draft's own attachment

- Classification: GUI parity
- Source anchor: `src/selector.rs`
- Daemon surface: `draft.materialize_attachment`, opened client-side
- GUI location: TBD (Phase 9)
- Validation: `tests/cli_selector_contract.rs`
- Status: not started

### ATT-05 Open a message's HTML part in the browser

- Classification: GUI parity
- Source anchor: TUI `tb` (`src/tui/app/keymap.rs:633`), search overlay `b`
- Daemon surface: `message.materialize` for the `.html` companion, opened client-side
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/parse.rs` for the companion document
- Status: not started

## Sending, outbox, and the undo hold

### SND-01 Send one approved draft

- Classification: GUI parity
- Source anchor: `mp send <selector> [-y]`, `src/main.rs`, `src/send.rs`
- Daemon surface: `send.draft`
- GUI location: TBD (Phase 9)
- Validation: `tests/outbox_integration.rs`, `tests/mime_oracle_integration.rs`, `tests/daemon_send_slice.rs`
- Status: routed (P4-U12); GUI not started
- Note: the preview and the `[y/N]` prompt stay in the client, which renders them from `draft.preview`: a daemon has no stdin, and a run without `-y` prints `Cancelled.` and exits 0 without a single `send.*` call.

### SND-02 Send every approved draft of one account or of all accounts

- Classification: GUI parity
- Source anchor: `mp send-approved [-y] [--all-accounts]` (`src/main.rs`), TUI `cX` (`src/tui/app/keymap.rs:669`)
- Daemon surface: `send.approved`
- GUI location: TBD (Phase 9)
- Validation: `tests/outbox_integration.rs`, `tests/daemon_send_slice.rs`
- Status: routed (P4-U12); GUI not started
- Note: `--all-accounts` is a loop in the client over `global_config.accounts` in configuration order, so `send.approved` names one account and a caller that sends `all_accounts` is refused.

### SND-03 Approve and send the current draft with one key

- Classification: GUI parity
- Source anchor: TUI `x` (`src/tui/app/keymap.rs:573`)
- Daemon surface: `draft.approve` then `send.draft`
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started

### SND-04 Undo-send hold with a visible countdown and a cancel key

- Classification: GUI parity
- Source anchor: `email.send_hold_secs` with a 20 second default, consumed at `src/tui/actions.rs:1231`
- Daemon surface: `send.hold_status`, `send.cancel_hold`, countdown on `state.event`
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/tui/actions.rs`
- Status: not started
- Note: the hold lives inside the TUI process today and `mp send` and `mp send-approved` bypass it (`ANO-7`); the migration moves it into the daemon in Phase 6, after the parity gate, keeps the CLI commands sending immediately, and lets the TUI and GUI observe and cancel the same countdown.
  When the last client exits mid-hold the daemon cancels the hold and leaves the draft approved, which is what killing the TUI does today.

### SND-05 Send a calendar invitation

- Classification: GUI parity
- Source anchor: `mp send --invite --to --cc --subject --start --end|--duration --location --description`, `src/main.rs`, `src/calendar.rs`
- Daemon surface: `send.invite`, refused on Microsoft Graph with the reason in the error
- GUI location: TBD (Phase 9)
- Validation: `tests/imip_integration.rs`, `tests/daemon_send_slice.rs`
- Status: routed (P4-U12); GUI not started
- Note: start and end accept local time or RFC3339, duration accepts ISO8601 or the short form, and Graph accounts are refused by `mailypoppins::invite::plan_invite`, which both the client and `send.invite` validate through, so the GUI shows a disabled action with its reason rather than a late failure (`ANO-4`); `send.invite` makes that refusal before it looks at anything else about the invitation.
  The client mints the `UID` while it previews and sends it as a parameter, so the UID a user read is the UID that goes out.

### SND-06 Outbox state visibility

- Classification: GUI parity
- Source anchor: `src/outbox.rs`, surfaced as a TUI badge and status entry
- Daemon surface: `state.bootstrap` outbox summary, then `state.event`
- GUI location: TBD (Phase 9)
- Validation: `tests/outbox_integration.rs`, `tests/daemon_send_slice.rs`
- Status: routed (P4-U12) for the `mp outbox list` surface; the TUI badge and GUI not started
- Note: covers queued, retrying, failed, and partly delivered submissions.

### SND-07 Outbox operator actions

- Classification: daemon administration
- Source anchor: `mp outbox list`, `mp outbox retry <id>`, `mp outbox discard <id>`, `src/main.rs`, `src/outbox.rs`, `tests/outbox_integration.rs`
- Daemon surface: `send.outbox_list`, `send.outbox_retry`, `send.outbox_discard`
- GUI location: TBD (Phase 9)
- Validation: `tests/outbox_integration.rs`, `tests/daemon_send_slice.rs`
- Status: routed (P4-U12); GUI not started
- Note: deliberately manual, because a submission that died without a verdict may or may not have been delivered, so the GUI shows the blocked state and names the command instead of guessing.
  `send.outbox_retry` is an operation and not a command: it re-arms the row and then drains it against SMTP and IMAP.

### SND-08 Partly delivered submission where a recipient was refused

- Classification: GUI parity for the surfacing
- Source anchor: `src/outbox.rs`, `src/send.rs`
- Daemon surface: `state.event` carrying the partly delivered state
- GUI location: TBD (Phase 9)
- Validation: `tests/outbox_integration.rs`, `tests/daemon_send_slice.rs`
- Status: routed (P4-U12) for the CLI surface; the `state.event` half and the GUI not started
- Note: only a human can close this state, and the GUI must not present it as a plain failure.

### SND-09 Sent-copy append after submission

- Classification: GUI parity
- Source anchor: `src/imap_client/sent.rs`
- Daemon surface: daemon-internal, reported on `state.event`
- GUI location: TBD (Phase 9)
- Validation: `tests/outbox_integration.rs`, `tests/daemon_send_slice.rs`
- Status: routed (P4-U12) for the CLI surface; the `state.event` half and the GUI not started
- Note: implicit workflow driven on the next startup or sync, which is how the outbox drives itself.

## Sync, offline behaviour, and pending operations

### SYN-01 Quick sync and full sync from a client

- Classification: GUI parity
- Source anchor: TUI `ss` (`src/tui/app/keymap.rs:593`) and `sS` (`src/tui/app/keymap.rs:594`)
- Daemon surface: `sync.quick`, `sync.full` as `operation.*` with progress on `state.event`
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames; the two methods in `tests/daemon_sync_slice.rs`
- Status: routed (P4-U10); GUI not started
- Note: both are operations rather than commands, and both are durable: a sync a GUI started keeps running, and stays watchable, from the CLI window beside it. `sync.full` takes no `limit`, because a bounded full pass is a quick pass under another name.

### SYN-02 Sync command options

- Classification: CLI automation
- Source anchor: `mp sync [-n] [--mailbox ...] [--dry-run] [--all-accounts]`, `src/main.rs`, `src/sync/engine.rs`
- Daemon surface: `sync.quick` carrying `limit`, `mailbox` and `dry_run`
- GUI location: TBD (Phase 9)
- Validation: `tests/cli_help_snapshot.rs`, `tests/daemon_sync_slice.rs`
- Status: routed (P4-U10); GUI not started
- Note: `--all-accounts` conflicts with `-A` by construction so a cron line cannot silently sync accounts it never named, and that conflict is a contract to preserve (`ANO-9`). `--all-accounts` itself is not on the wire: the client issues one operation per account in configuration order, because the per-account header, the failure denominator and the exit code are all rendering of a per-account result. `mp sync` always calls `sync.quick`, since `-n` has a default and the command has no unbounded form.

### SYN-03 Watch a mailbox through IMAP IDLE

- Classification: daemon administration
- Source anchor: `mp watch [--mailbox] [--timeout N]` with exit code 2 on timeout, `src/main.rs`, `src/imap_client/watch.rs`, `imap_watch` (`src/tui/helpers.rs:45`)
- Daemon surface: `sync.watch` as a client-scoped operation over the daemon's watcher
- GUI location: TBD (Phase 9)
- Validation: manual, requires a live server; validation and narrowing in `tests/daemon_sync_slice.rs`
- Status: routed (P4-U10); GUI not started
- Note: the on-demand IDLE connection was **not** built and the narrowing of `mp watch --mailbox` to INBOX is recorded in `BACKLOG.md` (P4-U10 took the route the plan recommends). Both sides carry it: the client warns on stderr and rewrites the mailbox before it calls, and the daemon refuses anything but INBOX with `-32602`. `--timeout N` stays client-side (wait, `operation.cancel`, `ℹ Timed out.`, exit 2), because a daemon-side timer would be a second place that knows about one client's patience.

### SYN-04 Startup refresh, asynchronous store open, and background mailbox load

- Classification: GUI parity
- Source anchor: the `Fetch`, `FetchAccount`, and `LoadMailbox` actions in `src/tui/app/types.rs` (`LoadMailbox` at `src/tui/app/types.rs:1611`)
- Daemon surface: `state.bootstrap` returning zeroed counts for an `opening` account, filled by `state.event`
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started
- Note: implicit workflow with no command, and the reason a client shows content before sync completes.

### SYN-05 Sync health and error surfacing per account

- Classification: GUI parity
- Source anchor: `src/sync_health.rs`
- Daemon surface: `state.bootstrap` health summary, then `state.event`
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/sync_health.rs`
- Status: not started

### SYN-06 Pending operation queue replayed against the server

- Classification: GUI parity
- Source anchor: `src/pending_ops.rs`, `src/ops.rs`
- Daemon surface: `state.event` for queue depth and outcomes; the drains of a `sync.*` pass as `operation.progress`
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/pending_ops.rs`
- Status: routed (P4-U10) for the sync tick's drains; GUI not started

### SYN-07 Store ingest, reconciliation, and drop-and-rebuild on an unreadable SQLite file

- Classification: diagnostics and maintenance
- Source anchor: `src/ingest.rs`, `src/reconcile.rs`, `src/store/rebuild.rs`, `tests/store_ingest_integration.rs`
- Daemon surface: daemon-internal, reported through `diagnostic.*`
- GUI location: TBD (Phase 9)
- Validation: `tests/store_ingest_integration.rs`
- Status: not started
- Note: the durable outbox survives a rebuild, which is the invariant this capability must not break.

### SYN-08 Retention garbage collection over cached blobs

- Classification: diagnostics and maintenance
- Source anchor: `mp store gc [--dry-run] [--force] [--all-accounts]`, `src/main.rs`, `src/store/sweep.rs`
- Daemon surface: `diagnostic.store_gc`, plus the automatic sweep the daemon runs after every sync
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/store/sweep.rs`, `tests/daemon_admin_slice.rs` (`mp_store_gc_matches_the_oracle`)
- Status: routed (P4-U14)
- Note: two safety rules a daemon or GUI port reproduces rather than relaxing (`ANO-5`): the first over-cap run warns and records a marker while the second evicts, and a plan reclaiming more than half the store's blob bytes is refused without `--force`.
  The sweep skips blobs backing a materialized handle a client still holds (`ANO-6`).

### SYN-09 Per-account engine lock

- Classification: daemon administration
- Source anchor: `src/engine_lock.rs`, acquired by `drain_account` (`src/pending_ops.rs:567`) for the mutation queue, by `drain_guarded_at` (`src/outbox.rs:1422`) for the outbox and by `run_sync_guarded_at` (`src/sync/engine.rs`) for the IMAP sync ingest
- Daemon surface: daemon-internal; the account runtime holds it for its lifetime
- GUI location: TBD (Phase 9)
- Validation: `tests/outbox_integration.rs`, `tests/engine_lock_ingest.rs`, unit tests in `src/engine_lock.rs`
- Status: not started
- Note: the outbox acquisition is #0116, where a refused drain reports nothing done and opens no session and the holder re-sweeps against a re-read clock.
  Phase 3b (#0122) extended it to the ingest half: `sync::engine::run_sync_guarded` is what `mp sync` and the TUI tick call, a non-holder returns `Ok(None)` before the transport is touched, and both callers report the refusal as information and succeed.
  `run_and_settle` (`src/pending_ops.rs:614`), `run_sync` itself and the Graph loop still take no lock, so `ANO-13` is closed for the IMAP ingest only.

### SYN-10 Microsoft Graph backend for sync and send

- Classification: GUI parity as a backend variant
- Source anchor: `src/graph.rs`
- Daemon surface: the same `sync.*` and `send.*` families over the Graph backend
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/graph.rs`
- Status: not started
- Note: delta cursors replace IMAP UID state, and invitation send is unavailable on this backend as recorded in `SND-05`.

### SYN-11 Offline operation

- Classification: GUI parity
- Source anchor: the store read paths and the draft editing paths, which do not require a server
- Daemon surface: `state.event` carrying connectivity; the GUI reaches this state through events rather than by falling back to direct store access
- GUI location: TBD (Phase 9)
- Validation: `tests/cli_read_surface_integration.rs`, which runs offline
- Status: not started

### SYN-12 Per-mailbox body-fetch deadline

- Classification: GUI parity for its surfacing
- Source anchor: `[imap] body_fetch_deadline_secs` (`src/config.rs:240`), clamped to 600 at load (`src/config.rs:958`), documented at `website/src/pages/config.astro`; `BODY_CHUNK_SIZE` (`src/imap_client/fetch.rs:525`); `bodies_complete` (`src/sync/mod.rs:167`); the TUI message at `src/tui/helpers.rs:411`
- Daemon surface: `state.event` progress carrying the deadline stop
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/imap_client/fetch.rs`, `src/tui/helpers.rs`
- Status: routed (P4-U10) for `mp sync`, which passes no deadline; GUI not started
- Note: default 30, `0` unbounded; bodies go out newest-first in chunks of 20 with the deadline checked between chunks and never inside a command, and the first chunk always goes out so an expired deadline still makes progress.
  A deadline stop returns `bodies_complete = false`, which defers the prune and the modseq like any other short pass and resumes on the next tick, and the client reports it as progress rather than failure (#0113).

### SYN-13 Tail drain of the outbox and the mutation queue after the sync body

- Classification: GUI parity for its surfacing
- Source anchor: `run_tick_with_drains` (`src/sync/tick.rs:25`), driven by both TUI tick paths and by `mp sync` (`src/main.rs:1405`); the non-fatal head-drain error at `src/main.rs:1348`
- Daemon surface: one `operation.progress` per phase, its `phase` naming which of the five slots reported
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/sync/tick.rs`; the wordings and the label in `tests/daemon_sync_slice.rs`
- Status: routed (P4-U10); GUI not started
- Note: tail report lines carry an " (after sync)" label so they cannot be read as the head's, and a head-drain error prints a warning and continues instead of aborting the sync; the daemon owns the tick after the cutover, so this ordering and this non-fatal error are contracts (#0114). The label is the client's and is derived from the phase name alone (`Phase::as_str`), so the daemon publishes facts and `mp_client::format` decides the words.

### SYN-14 Per-mailbox non-convergence detector

- Classification: GUI parity for its surfacing
- Source anchor: `NONCONVERGING_PREFIX` (`src/sync/engine.rs:34`) over a `nonconverging:{role}` row in the store's meta table; the `mp sync` line at `src/main.rs:1506`; `NON_CONVERGING_MARKER` (`src/tui/helpers.rs:326`) and the status downgrade in `src/tui/bg.rs:12`
- Daemon surface: `state.event` warning carrying the marker
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/sync/engine.rs`
- Status: not started
- Note: the meta row holds `hash:count:streak` for the pass's UID set and warns at streak 2, at 3, and every tenth thereafter, on a still-zero exit code.
  A truncated, incomplete, reset, dry-run, or given-up pass neither counts nor resets, and the Graph loop has no detector, so a client must not present its absence there as convergence (#0115).

### SYN-15 Outbox drain exclusivity under the engine lock

- Classification: daemon administration
- Source anchor: `drain_guarded` (`src/outbox.rs:1401`) and `drain_guarded_at` (`src/outbox.rs:1414`), `send::drain_account` (`src/send.rs:2197`), `tests/outbox_integration.rs`
- Daemon surface: daemon-internal
- GUI location: TBD (Phase 9)
- Validation: `tests/outbox_integration.rs`
- Status: not started
- Note: a drain refused the lock does nothing, opens no session, and is a success rather than an error, and the holder re-sweeps up to four times against a clock re-read per sweep (#0116).

### SYN-16 UIDVALIDITY-reset unbind of unverified rows

- Classification: diagnostics and maintenance
- Source anchor: `unbind_rows_on_uids` (`src/ingest.rs:822`), run after the ingest loop of a pass that reported a reset
- Daemon surface: daemon-internal, reported in the `sync.*` result
- GUI location: TBD (Phase 9)
- Validation: `tests/store_ingest_integration.rs`, unit tests in `src/ingest.rs`
- Status: not started
- Note: every row still parked on a listed UID the pass did not itself ingest moves to the `-id` sentinel, which frees the UID for the message that now wears it and leaves the row rebindable; rows on UIDs the server does not list are left alone, and recovery completes on the next full sync because the download window is positional and the repaired rows sit below it (#0117).

## Contacts

### CON-01 Fuzzy contact search over name and address

- Classification: GUI parity
- Source anchor: `mp contacts search [query] [-n] [--account]` (`src/main.rs`), the TUI contacts view `/`, `src/contacts/matcher.rs`
- Daemon surface: `contact.search`
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/contacts/matcher.rs`, `tests/daemon_admin_slice.rs`
- Status: routed (P4-U14)

### CON-02 Tab-delimited `email` and `name` output for mutt, aerc, and vim

- Classification: CLI automation
- Source anchor: `mp contacts search --parsable`, `src/main.rs`
- Daemon surface: `contact.search` with the parsable projection
- GUI location: TBD (Phase 9)
- Validation: `tests/cli_help_snapshot.rs`, `tests/daemon_admin_slice.rs` (`mp_contacts_search_parsable_is_tab_delimited`)
- Status: routed (P4-U14)
- Note: a stable shape other tools already consume, so the daemon migration must not reformat it (`ANO-8`).
  `contact.search` answers rows named `{address, display_name, sent_to, sent_cc, received, score}`, so the tab-delimited line is a projection of the wire row rather than a translation of it.

### CON-03 Rebuild or refresh the contact index from the local message store

- Classification: GUI parity
- Source anchor: `mp contacts rebuild [--account]` (`src/main.rs`), the TUI contacts view `r`
- Daemon surface: `contact.rebuild` as an `operation.*`
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/contacts/`, `tests/daemon_admin_slice.rs`
- Status: routed (P4-U14)
- Note: the all-accounts default of the CLI form is automation, while the single-account refresh is the user-facing capability.
  The loop is the client's, over the configured accounts in configuration order; the method takes one required `account` and no `all_accounts`.

### CON-04 Contact index statistics

- Classification: diagnostics and maintenance
- Source anchor: `mp contacts stats [--account]`, `src/main.rs`
- Daemon surface: `contact.stats`
- GUI location: TBD (Phase 9)
- Validation: `tests/cli_help_snapshot.rs`, `tests/daemon_admin_slice.rs`
- Status: routed (P4-U14)

### CON-05 Compose to a contact from the contacts view

- Classification: GUI parity
- Source anchor: TUI `Enter` and `n` in the CONTACTS keymap section
- Daemon surface: `draft.create` seeded from the contact
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started

### CON-06 Send a contact as a vCard

- Classification: GUI parity
- Source anchor: TUI `v` in the contacts view, `src/contacts/vcard.rs`
- Daemon surface: `contact.vcard`, then `draft.create`
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/contacts/vcard.rs`
- Status: not started

### CON-07 Copy a contact's email address

- Classification: GUI parity
- Source anchor: TUI `c` in the CONTACTS keymap section
- Daemon surface: client-side clipboard write
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started

### CON-08 Address extraction and frecency ranking from the message store

- Classification: GUI parity
- Source anchor: `src/contacts/extractor.rs`, `src/contacts/rank.rs`, `src/contacts/hooks.rs`
- Daemon surface: daemon-internal, with `state.event` when the index changes
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/contacts/rank.rs`, `src/contacts/extractor.rs`
- Status: routed (P4-U14)
- Note: implicit workflow that keeps the index current as mail arrives.
  Since the admin slice the extraction, the ranking and the cache guard (#0067) all run in the daemon; no client opens the index.

## Calendar and iMIP

### CAL-01 RSVP to a received invitation

- Classification: GUI parity
- Source anchor: `mp invite accept|tentative|decline <selector> [--mailbox]` (`src/main.rs`), TUI `tv` in the message context and `V` in the calendar view, `src/invite.rs`, `tests/imip_integration.rs`
- Daemon surface: `calendar.rsvp` as an `operation.*`
- GUI location: TBD (Phase 9)
- Validation: `tests/imip_integration.rs`, `tests/daemon_admin_slice.rs` (`mp_invite_refusals_match_the_oracle`, and the successful reply through the daemon's fake transport)
- Status: routed (P4-U14)
- Note: whole-series only in v1, the reply travels as iMIP over SMTP, and the target message must carry an `invite.ics` blob.
  The Graph refusal (`ANO-4`) is made before anything about the selector is examined, so a surface that shows the RSVP buttons disabled can say why without naming a resolvable message.

### CAL-02 Agenda view with an upcoming and past toggle and a refresh

- Classification: GUI parity
- Source anchor: TUI `t` and `r` in the calendar view, `src/tui/app/calendar_view.rs`, `src/tui/ui/calendar.rs`
- Daemon surface: `calendar.agenda`
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/tui/app/calendar_view.rs`, TUI golden frames
- Status: not started

### CAL-03 Open the source email of an agenda entry

- Classification: GUI parity
- Source anchor: TUI `Enter` and `e` in the calendar view
- Daemon surface: `message.get`, then `message.materialize` for the editor session
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started

### CAL-04 Report what stored attendee replies resolve on stored invitations

- Classification: diagnostics and maintenance
- Source anchor: `mp calendar rebuild [--account]`, `src/main.rs`, `src/calendar_cmd.rs`
- Daemon surface: `calendar.rebuild`, which reports and writes nothing
- GUI location: TBD (Phase 9)
- Validation: `tests/imip_integration.rs`, `tests/daemon_admin_slice.rs` (`mp_calendar_rebuild_matches_the_oracle`)
- Status: routed (P4-U14)
- Note: attendee status is derived from the `invite.ics` payloads wherever it is displayed, so there is no cached copy to rebuild.

### CAL-05 Invitation rendering with derived attendee statuses

- Classification: GUI parity
- Source anchor: `src/invite.rs`
- Daemon surface: `calendar.agenda` and `message.get` carry the derived statuses
- GUI location: TBD (Phase 9)
- Validation: `tests/imip_integration.rs`, unit tests in `src/invite.rs`
- Status: not started

### CAL-06 Invitation updates and cancellations reflected in the agenda and the reader

- Classification: GUI parity
- Source anchor: implicit workflow driven by newly synced iMIP messages
- Daemon surface: `state.event`
- GUI location: TBD (Phase 9)
- Validation: `tests/imip_integration.rs`
- Status: not started

## Client-side integrations

### INT-01 Open `config.toml` in the editor

- Classification: GUI parity
- Source anchor: TUI `sc` (`src/tui/app/keymap.rs:596`)
- Daemon surface: `config.path` for the location; the daemon reloads the file either way
- GUI location: TBD (Phase 9)
- Validation: manual
- Status: not started
- Note: the GUI equivalent is the settings surface plus an explicit reveal or open action.

### INT-02 Open the log file in the editor

- Classification: GUI parity
- Source anchor: TUI `sf` (`src/tui/app/keymap.rs:597`), the `OpenLogFile` action (`src/tui/app/types.rs:1597`)
- Daemon surface: `diagnostic.log_path`
- GUI location: TBD (Phase 9)
- Validation: manual
- Status: not started
- Note: the GUI provides a log view plus an explicit reveal or open-in-editor action.

### INT-03 Clipboard writes for selectors, paths, and addresses

- Classification: GUI parity
- Source anchor: the clipboard actions in `src/tui/actions.rs`
- Daemon surface: client-side in every client
- GUI location: TBD (Phase 9)
- Validation: manual
- Status: not started

### INT-04 Browser launch for HTML parts and OAuth verification URLs

- Classification: GUI parity
- Source anchor: the browser actions in `src/tui/actions.rs`, `src/config_cmd/oauth2.rs`
- Daemon surface: client-side in every client
- GUI location: TBD (Phase 9)
- Validation: manual
- Status: not started

### INT-05 Desktop notifications for new mail

- Classification: GUI parity
- Source anchor: `src/notify.rs`, using osascript on macOS and notify-send on Linux with sanitized payloads
- Daemon surface: the daemon decides a notification is warranted and emits `state.event`; the client holding the entitlement presents it
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/notify.rs`
- Status: not started

### INT-06 Editor suspension and resume around an external `$EDITOR`

- Classification: GUI parity
- Source anchor: `Action::suspends_terminal` (`src/tui/app/types.rs:1716`) and the suspend path in `src/tui/mod.rs`
- Daemon surface: client-side
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/tui/app/types.rs`
- Status: not started
- Note: the GUI replaces suspension with the embedded PTY session.

## Status, activity, logging, and help

### OBS-01 Activity log overlay with scrolling and a filter

- Classification: GUI parity
- Source anchor: TUI `!` (`src/tui/app/keymap.rs:570`), `sl` (`src/tui/app/keymap.rs:595`), and `/` inside the overlay (ACTIVITY LOG keymap section)
- Daemon surface: `state.event` activity stream; the overlay itself is client-side
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started

### OBS-02 Command palette over every runnable action

- Classification: GUI parity
- Source anchor: TUI `:` (`src/tui/app/keymap.rs:564`) and `Ctrl+p` (`src/tui/app/keymap.rs:565`), `palette_actions()` (`src/tui/app/keymap.rs:841`)
- Daemon surface: client-side, derived from `KEYMAP` so it cannot drift from the bindings
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/tui/app/keymap.rs`, TUI golden frames
- Status: not started

### OBS-03 Help overlay and hint bar

- Classification: GUI parity
- Source anchor: TUI `?` (`src/tui/app/keymap.rs:561`), `help_sections()` (`src/tui/app/keymap.rs:787`)
- Daemon surface: client-side, generated from the same `KEYMAP`
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/tui/app/keymap.rs`, TUI golden frames
- Status: not started

### OBS-04 Status line carrying counts, badges, operation progress, and persistent errors

- Classification: GUI parity
- Source anchor: `src/tui/ui/status.rs`
- Daemon surface: `state.bootstrap` summaries, then `state.event` and `operation.*` progress
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/tui/ui/status.rs`, TUI golden frames
- Status: not started

### OBS-05 Structured logging into the platform log directory

- Classification: diagnostics and maintenance
- Source anchor: `src/timing.rs`, the `OpenLogFile` action (`src/tui/app/types.rs:1597`)
- Daemon surface: `diagnostic.log_path`; the daemon writes its own log
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/timing.rs`
- Status: not started

### OBS-06 Dump the key bindings as Markdown or JSON

- Classification: CLI automation
- Source anchor: `mp dump-keys [--json]`, `src/tui/app/keymap.rs`, `scripts/regen-website-keys.sh` feeding `website/src/data/tui-keys.json`
- Daemon surface: none; it needs no daemon
- GUI location: TBD (Phase 9)
- Validation: `docs/baselines/pre-daemon/tui-keys.json`, byte-identical to the website copy
- Status: not started
- Note: the data feed the GUI key help reuses rather than duplicating (`ANO-8`), and the artifact that goes stale against a newer `keymap.rs` unless it is regenerated after `cargo install --path .` (`ANO-1`).

### OBS-07 Theme configuration

- Classification: GUI parity with a recorded deferral
- Source anchor: the top-level `theme` key in `config.toml` (`src/config.rs:25`), `src/tui/theme.rs`, `docs/tickets/0023-enable-theme-config.md`
- Daemon surface: client-side; the client reads the theme at startup
- GUI location: TBD (Phase 9)
- Validation: `test_parse_config_with_theme` (`src/config.rs:1602`)
- Status: deferred, with the light theme in the deferred backlog
- Note: read once at startup, so a change needs a restart; the first GUI release is dark-only by settled decision.

### OBS-08 Quit the client while leaving durable work in place

- Classification: GUI parity
- Source anchor: TUI `q` (`src/tui/app/keymap.rs:557`)
- Daemon surface: client disconnect; the daemon keeps running
- GUI location: TBD (Phase 9)
- Validation: TUI golden frames
- Status: not started
- Note: in the GUI, closing the window exits the client and leaves the daemon running.

## Daemon administration and lifecycle

Every capability in this group is new in this plan and has no current source anchor: no daemon, serve, or IPC surface exists in the tree, so this layer is greenfield and carries no legacy compatibility burden (`ANO-10`).

### LIF-01 Run the daemon in the foreground

- Classification: daemon administration
- Source anchor: none, new in this plan; entry point `mp daemon run`
- Daemon surface: the process itself, binding `<data_dir>/runtime/daemon.sock`
- GUI location: TBD (Phase 9)
- Validation: none today
- Status: not started

### LIF-02 Start a detached daemon and wait for readiness

- Classification: daemon administration
- Source anchor: none, new in this plan; entry point `mp daemon start`
- Daemon surface: `daemon.start.lock` for startup exclusion, then a handshake on the socket
- GUI location: TBD (Phase 9)
- Validation: none today
- Status: not started

### LIF-03 Report daemon status, version, instance, and account health

- Classification: daemon administration
- Source anchor: none, new in this plan; entry point `mp daemon status`
- Daemon surface: `daemon.status` over `daemon.json`
- GUI location: TBD (Phase 9)
- Validation: none today
- Status: not started

### LIF-04 Stop the daemon gracefully

- Classification: daemon administration
- Source anchor: none, new in this plan; entry point `mp daemon stop`
- Daemon surface: `daemon.stop`, naming the operations that prevented a clean stop
- GUI location: TBD (Phase 9)
- Validation: none today
- Status: not started

### LIF-05 Restart the daemon explicitly

- Classification: daemon administration
- Source anchor: none, new in this plan; entry point `mp daemon restart`
- Daemon surface: `daemon.stop` then a fresh start; exit code 3 names this command on an incompatible daemon
- GUI location: TBD (Phase 9)
- Validation: none today
- Status: not started
- Note: also invoked by the GUI mismatch screen after user confirmation.

### LIF-06 Install or remove the login-start service

- Classification: daemon administration
- Source anchor: none, new in this plan; launchd and systemd user units
- Daemon surface: `daemon.install_service`, `daemon.remove_service`
- GUI location: TBD (Phase 9)
- Validation: none today
- Status: not started

### LIF-07 Health and support diagnostics

- Classification: diagnostics and maintenance
- Source anchor: none, new in this plan
- Daemon surface: the `diagnostic.*` family
- GUI location: TBD (Phase 9)
- Validation: none today
- Status: not started

### LIF-08 On-demand automatic start from any normal client

- Classification: daemon administration
- Source anchor: none, new in this plan
- Daemon surface: the client spawns `mp daemon run` and waits for readiness; exit code 4 on failure
- GUI location: TBD (Phase 9)
- Validation: none today
- Status: not started
- Note: excludes the lifecycle commands themselves and the commands that read no domain state (`ACC-04`, `OBS-06`).

## Migration-only surfaces

### MIG-01 Report what remains of the file-era `.md` tree and import its drafts

- Classification: migration-only
- Source anchor: `mp cutover [--account] [--dry-run]`, `src/main.rs`, `src/cutover.rs`
- Daemon surface: `config.cutover` as an `operation.*`
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/cutover.rs`, `tests/daemon_admin_slice.rs` (`mp_cutover_matches_the_oracle`)
- Status: routed (P4-U14)
- Note: P4-U14 took the first of the two options this row offered; the client-side one is gone, because after the cutover the daemon owns the data directory and the drafts index the import writes into.
- Note: assigns an `id:` field to any draft lacking one so it becomes addressable by selector, names the dead mailbox directories, prints the command that removes them, and deletes nothing itself, while `--dry-run` writes not even the `id:` field.

### MIG-02 Signature migration from the `[accounts.signatures]` TOML block into signature files

- Classification: migration-only
- Source anchor: `migrate_config_signatures` (`src/signatures.rs:265`), with the default recorded in `state.json`
- Daemon surface: runs in the daemon startup sequence after the cutover
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/signatures.rs`
- Status: not started

### MIG-03 Store schema migration at account-runtime start

- Classification: migration-only
- Source anchor: `src/store/schema.rs`
- Daemon surface: runs inside the daemon at account-runtime start, so no client performs it
- GUI location: TBD (Phase 9)
- Validation: `tests/store_ingest_integration.rs`
- Status: not started

### MIG-04 Legacy config-directory migration

- Classification: migration-only
- Source anchor: `migrate_legacy_config_dir` (`src/config.rs:668`), called from `src/main.rs:1672` on every command
- Daemon surface: runs once in the daemon startup sequence, before the first configuration load
- GUI location: TBD (Phase 9)
- Validation: unit tests in `src/config.rs`
- Status: not started
- Note: an explicit `MAILYPOPPINS_CONFIG_DIR` (`src/config.rs:606`) suppresses the fallback, which is why it belongs to daemon identity and not only to startup; identity is the pair with `MAILYPOPPINS_DATA_DIR` (`src/config.rs:1099`) and a mismatch is refused at the handshake (`ANO-14`).

## Anchor corrections

Every anchor above was resolved against the tree at `f8af44b`.
Four moved or were imprecise in the plan's inventory and are corrected here.

- `MBX-04`: `src/tui/app/keymap.rs:592` is a section comment; the `ga` binding is at line 591.
- `ACC-11`: the `{{SIGNATURE}}` marker is not in `src/signatures.rs`; the splice and strip logic lives at `src/send.rs:185-268`, with the preview substitution at `src/tui/ui/preview.rs:71`.
  `src/signatures.rs` holds the file and default lookup only.
- `MSG-08`: `queue_mark_open_read` is defined at `src/tui/app/mod.rs:1179`; `src/tui/app/keys.rs` calls it (lines 305 and 319), which is what the inventory's wording describes.
- `OBS-07`: there is no `[theme]` config section; `theme` is a top-level key (`src/config.rs:25`).
  `src/tui/theme.rs` resolves.

`RD-05` has no resolvable anchor by design: `src/tui/images.rs` was deleted when #0109 retired the capability.

`SYN-15` cites `src/outbox.rs:1401` for `drain_guarded`, which is correct; `drain_guarded_at` begins at line 1414, and `SYN-09`'s `src/outbox.rs:1422` points at the lock acquisition inside it.
Both resolve as written.

## Capabilities the inventory does not name

Cross-reading `docs/baselines/pre-daemon/cli-help.txt` (every subcommand of every command) and `docs/baselines/pre-daemon/tui-keys.json` (92 bindings across 10 sections) against the entries above leaves no unnamed CLI surface.
Three key surfaces are only implicitly covered.

- `Esc` in the MESSAGE context, which clears the selection and returns to the list, falls under `MSG-06` without being named there.
  The GUI needs an equivalent escape.
- Half-page `d/u` in the SERVER SEARCH and ACTIVITY LOG overlays sits outside `LST-02`, which names half-page scrolling for the list and body panes only.
- `Tab` inside the SERVER SEARCH overlay switches focus between the result list and the query field, which is overlay-internal focus rather than the global pane cycling of `MBX-06`.

The overlay-internal keys `mp dump-keys` cannot see are inventoried by P0-U2 in `docs/baselines/pre-daemon/manual-keys.md` (`ANO-2`) rather than here.
