# mailypoppins desktop client

The macOS GUI of mailypoppins, a third client of the daemon beside the CLI and the TUI.
It is built with Tauri 2, React 19, Vite, Tailwind 4 and shadcn on Base UI.
The plan is [docs/plans/native-gui.md](../../docs/plans/native-gui.md), and the tickets are [#0129](../../docs/tickets/0129-gui-shell-and-design-system.md) for M1 and [#0131](../../docs/tickets/0131-gui-full-parity.md) for M2 to M4.

## What works

- M1: accounts, mailboxes with counts and sync health, the message list, the sandboxed reader, and local and server search.
- M2: archive, delete, move, flag and read, on one row or on marked rows, applied at once and put back on a refusal or a rollback, with the TUI's keys, palette entries, row toggles and a reader toolbar.
- M2: the archive and delete confirmation, the move picker, mark read on an explicit open, quick and full sync, and the activity area with the send-hold countdown and Cancel.
- M3: new drafts through a wizard, reply, reply all and forward, with the draft opened in the external editor and each save shown in the Drafts list.
- M3: approve and demote, the draft preview with its validation and attachments, and the recipients dialog.
- M3: send and send all approved behind the TUI's confirmations and the daemon's hold, with the Sent, Send cancelled, Failed and Partly delivered outcomes.
- M3: the outbox view with retry and discard, the sidebar's outbox line as its link, and the queue depth in the status region.
- M3: opening and saving a message's attachments, the browser rendition, attaching a file to a draft, and fetching a server-only search hit; paths are typed until the dialog plugin brings the native picker.
- M4: the Contacts, Calendar and Settings views beside Mail, switched with `Space c`, `Space a` and `Space m`, the sidebar or the palette.
- M4: the agenda with its past toggle and the source `invite.ics` in the editor, the reader's invitation card, RSVP from the card, `tv` and `V`, and a New invitation form, disabled on a Graph account.
- M4: ranked contacts with a fuzzy search, compose to a contact, a contact sent as a vCard draft, the address copied, and the index rebuild.
- M4: the Signatures dialog on `cs`, the activity log on `sl`, config.toml and the daemon log in the editor on `sc` and `sf`, and the reader's Copy menu.
- M4: Settings with the config.toml banner, Reload, passwords and the device-code sign-in, the account wizard with the CLI's four presets, and a setup screen when the daemon has no config.toml.
- Nothing of M2 to M4 has run in a real window yet; the tests run against the fixtures.

The Rust crate `mp-desktop` in `src-tauri/` has its own workspace and `Cargo.lock`, since the root workspace excludes `clients/desktop`.
It links `mp-client`, `mp-protocol` and `mp-core` by path, never the root `mailypoppins` crate, and talks to the daemon only through `mp-client`, as `ClientKind::Gui`.

## Prerequisites

- macOS with the Xcode Command Line Tools (`xcode-select --install`).
- Rust stable, 1.90 or later for Tauri 2.12.
- pnpm, and Node.js 22.18 or later, which runs the `.ts` scripts directly.
- `mp` on `PATH`, or its path in `MP_DESKTOP_MP_BIN`; fixture mode needs neither.

## Run

```sh
cd clients/desktop
pnpm install
pnpm tauri dev                          # against the daemon, started with `mp daemon start` if none runs
MP_DESKTOP_FIXTURE=1 pnpm tauri dev     # on the bundled fixtures, no daemon
```

Fixture mode serves `fixtures/*.json`: 2 accounts, 6 mailboxes, 21 messages, 2 drafts, 3 HTML bodies, 1 armed send hold, 6 agenda events and 28 contacts, row 1006 being a hostile message.
It is the way to iterate on the screens without a mail server.
The drafts are real files in a per-run directory, and the fixture never starts an editor or the system opener: it records the path instead.
The `fixture_simulate` command, answered only in fixture mode, takes these:

- `editor_save` and `editor_invalid`: a save, or a broken frontmatter, in the file the last `editor_open` named.
- `send_fail`, `send_partial` and `send_pending_append`: the next send or outbox retry fails, refuses a recipient, or delivers and owes its Sent copy.
- `send_hold:<secs>`: the hold window of later sends, `0` for none, 10 s by default.
- `invite_update` and `invite_cancel`: a new version or a cancellation of the steering committee lands in `work`'s inbox; `rsvp_fail`: the next RSVP fails.
- `rebuild_refused`: the next contact rebuild is refused by the cache guard; `signature_changed`: the `work` signature is edited behind the app's back.
- `config_invalid`: the next reload finds a broken config.toml; `config_absent`: a daemon restarted on an empty configuration directory.
- `oauth_approve` and `oauth_deny`: the waiting device-code sign-in is approved or declined.
- `hold`, `rollback`, `rollback:<n>`, `disconnect`, `reconnect`, `restart`, `resync`, `new_mail` and `shutdown`, as before.

[docs/rust-layer.md](docs/rust-layer.md), "Fixture mode", has the details.

## Test and build

```sh
pnpm test                               # vitest under jsdom, the Tauri API mocked from the fixtures
pnpm build                              # type-check and bundle the frontend
pnpm gen:keymap                         # regenerate src/keymap/keymap.json from `mp dump-keys --json` (MP_BIN or `mp` on PATH)
pnpm gen:types                          # regenerate src/protocol/generated/ from the Rust types with ts-rs (runs cargo)
pnpm contrast                           # rewrite the contrast table in docs/design-tokens.md

cd src-tauri
export CARGO_TARGET_DIR=/var/tmp/mp-desktop-target
cargo test                              # the Rust layer; add `-- --ignored live_daemon` with MP_DESKTOP_MP_BIN set for the live test
cargo clippy --all-targets -- -D warnings
```

## Install from a local build

```sh
cd clients/desktop
pnpm bundle                             # builds mp in release, bundles it as the sidecar, runs tauri build
MP_SIDECAR_BIN=/path/to/mp pnpm bundle  # bundle an mp you already built
```

`pnpm bundle` ([scripts/bundle.ts](scripts/bundle.ts)) copies `mp` to `src-tauri/binaries/mp-<target>` and builds with [src-tauri/tauri.bundle.conf.json](src-tauri/tauri.bundle.conf.json), which names it in `bundle.externalBin`; plain `pnpm tauri build` makes an app without its own `mp`.
The app lands in `<target dir>/<target>/release/bundle/macos/mailypoppins.app` and the DMG beside it in `dmg/`, the target dir being `$CARGO_TARGET_DIR` or `src-tauri/target`.
The bundle is unsigned until #0012, so a downloaded copy needs one Control-click Open, or `xattr -dr com.apple.quarantine` on the app; a local build is not quarantined.

```sh
cp -R <target dir>/aarch64-apple-darwin/release/bundle/macos/mailypoppins.app /Applications/
open /Applications/mailypoppins.app
ln -s /Applications/mailypoppins.app/Contents/MacOS/mp /usr/local/bin/mp   # optional: the app's mp on PATH
```

The app starts its daemon with its own `Contents/MacOS/mp` unless `MP_DESKTOP_MP_BIN` names another, and refuses a running daemon of another version with the restart screen, whose Restart runs `mp daemon restart` with that binary.
The symlink keeps the CLI, the TUI and the app's daemon on one version; with a Homebrew or `cargo` `mp` earlier on `PATH`, the two versions meet at the restart screen.
To uninstall: `mp daemon stop`, quit the app, delete `/Applications/mailypoppins.app` and the symlink; the mail store and `config.toml` are the CLI's and stay (`mp config path`).
[docs/release-process.md](../../docs/release-process.md), "The desktop app", has the release side.

## Environment

| Variable | Effect |
|---|---|
| `MP_DESKTOP_FIXTURE=1` (or `--fixture`) | Serve the fixtures, no daemon |
| `MP_DESKTOP_MP_BIN` | The `mp` binary that starts the daemon and whose version the daemon must run; else the one next to the executable (the bundled sidecar), then `PATH`, then `~/.cargo/bin`, `/opt/homebrew/bin`, `/usr/local/bin` |
| `MP_DESKTOP_WINDOW_SIZE=WxH` | The initial window size, e.g. `950x800` for the medium layout or `600x820` for the narrow one |
| `MP_DESKTOP_STUB_OPENER=1` | "Open in browser" records the URL in the intercepted-URL log instead of opening it, and a file open only logs; automated runs set it |
| `MP_DESKTOP_EDITOR` | The editor command for drafts and every other file the app opens in an editor, `{path}` standing for the file; else the `editor` key of `desktop.json`, `$VISUAL` or `$EDITOR` unless a terminal editor, a probed `code`, `zed`, `subl` or `cursor`, then `open -t` |
| `MP_DESKTOP_LOG` | `error` to `trace`, default `info`, to stderr and `<data>/logs/mp-desktop.log` |

The daemon's own variables (`MAILYPOPPINS_DATA_DIR`, `MAILYPOPPINS_CONFIG_DIR`, `MAILYPOPPINS_DAEMON_AUTOSTART`) apply as they do to `mp`.

## Documentation

- [docs/rust-layer.md](docs/rust-layer.md): the Tauri commands, the generated TypeScript types, the event stream, the reader scheme, links, the app CSP and fixture mode.
- [docs/shell.md](docs/shell.md): the frontend modules, the model, mutations and pending state, the dialogs and the activity area, compose and send, the outbox, the views, contacts, the calendar, signatures, the activity log, settings, the account wizard, first run, the layouts, focus order and keys.
- [docs/reader.md](docs/reader.md): the reader frame, the toolbar and its Copy menu, invitations, the attachments, the browser rendition, the draft preview and server-only hits, refused links, and the navigation guard's verification with its manual steps.
- [docs/design-tokens.md](docs/design-tokens.md): the semantic tokens and their contrast table.
