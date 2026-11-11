# mailypoppins desktop client

The macOS GUI of mailypoppins, a third client of the daemon beside the CLI and the TUI.
It is built with Tauri 2, React 19, Vite, Tailwind 4 and shadcn on Base UI.
The plan is [.agents/plans/native-gui.md](../../.agents/plans/native-gui.md), and the tickets are PERSO-206 for M1 and PERSO-207 for M2 to M4.

## What works

- M1: accounts, mailboxes with counts and sync health, the message list, the sandboxed reader, and local and server search.
- M2: archive, delete, move, flag and read, on one row or on marked rows, applied at once and put back on a refusal or a rollback, with the TUI's keys, palette entries, row toggles and a reader toolbar.
- M2: the archive and delete confirmation, the move picker, mark read on an explicit open, quick and full sync, and the activity area with the send-hold countdown and Cancel.
- M3: new drafts through a wizard, reply, reply all and forward, with the draft opened in the external editor and each save shown in the Drafts list.
- M3: approve and demote, the draft preview with its validation and attachments, and the recipients dialog.
- M3: send and send all approved behind the TUI's confirmations and the daemon's hold, with the Sent, Send cancelled, Failed and Partly delivered outcomes.
- M3: the outbox view with retry and discard, the sidebar's outbox line as its link, and the queue depth in the status region.
- M3: opening and saving a message's attachments, the browser rendition, attaching a file to a draft, and fetching a server-only search hit; a path is typed or picked with Browse in the system's file or folder picker.
- M4: the Contacts, Calendar and Settings views beside Mail, switched with `Space c`, `Space a` and `Space m`, the sidebar or the palette.
- M4: the agenda with its past toggle and the source `invite.ics` in the editor, the reader's invitation card, RSVP from the card, `tv` and `V`, and a New invitation form, disabled on a Graph account.
- M4: ranked contacts with a fuzzy search, compose to a contact, a contact sent as a vCard draft, the address copied, and the index rebuild.
- M4: the Signatures dialog on `cs`, the activity log on `sl`, config.toml and the daemon log in the editor on `sc` and `sf`, and the reader's Copy menu.
- M4: Settings with the config.toml banner, Reload, passwords and the device-code sign-in, the account wizard with the CLI's four presets, and a setup screen when the daemon has no config.toml.
- Updates (#0139): an "Update to 0.12.0" entry at the sidebar's foot once a newer release is known, "Check for updates" in the palette, the App menu and Settings, the download's progress on a card in the activity area, and "Restart now" or "Later" once it is installed.
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

Fixture mode serves `fixtures/*.json`: 2 accounts, 6 mailboxes, 22 messages, 2 drafts, 3 HTML bodies, 1 armed send hold, 6 agenda events and 28 contacts, row 1006 being a hostile message.
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
- `reminder`: the first timed server event's reminder fires, presented by the app, as the daemon's `calendar.reminder` would say it.
- `hold`, `rollback`, `rollback:<n>`, `disconnect`, `reconnect`, `restart`, `resync`, `new_mail` and `shutdown`, as before.

[docs/rust-layer.md](docs/rust-layer.md), "Fixture mode", has the details.

```sh
pnpm preview:browser                    # the frontend in a browser at http://localhost:1430, on the tests' fixture backend
```

The browser preview needs no Rust build and serves the same fixtures through the TypeScript backend the tests use; `window.mpFixture` drives it.
[docs/browser-preview.md](docs/browser-preview.md) has what it covers, how it stays out of the bundle, and how to drive it.

## Test and build

```sh
pnpm test                               # vitest under jsdom, the Tauri API mocked from the fixtures
pnpm exec tsc -b                        # type-check the frontend and the Node side (vite.config.ts, scripts/), writing only under node_modules/.tmp
pnpm build                              # the same type-check, bundle the frontend, then check that no fixture or WebDriver code is in it
pnpm test:e2e                           # the end-to-end specs in the app's own window, on the fixtures (below)
pnpm gen:keymap                         # regenerate src/keymap/keymap.json from `mp dump-keys --json` (MP_BIN or `mp` on PATH)
pnpm gen:types                          # regenerate src/protocol/generated/ from the Rust types with ts-rs (runs cargo)
pnpm contrast                           # rewrite the contrast table in docs/design-tokens.md

cd src-tauri
export CARGO_TARGET_DIR=/var/tmp/mp-desktop-target
cargo test                              # the Rust layer; add `-- --ignored live_daemon` with MP_DESKTOP_MP_BIN set for the live test
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features e2e -- -D warnings   # the end-to-end tests' build
```

## End-to-end tests

```sh
cd clients/desktop
export CARGO_TARGET_DIR=/var/tmp/mp-e2e-target
pnpm test:e2e                                               # build, then run every spec
pnpm test:e2e --no-build                                    # run against the last build
pnpm test:e2e -- --spec e2e/specs/02-reader.e2e.ts          # anything after `--` goes to `wdio run`
```

[scripts/e2e.ts](scripts/e2e.ts) builds a debug app with the `e2e` cargo feature (`tauri build --debug --no-bundle --features e2e`, the frontend through `pnpm build`) into `<target dir>/debug/mp-desktop`, then runs the WebdriverIO specs in [e2e/specs/](e2e/specs/) against it with `@wdio/tauri-service`.
The feature embeds `tauri-plugin-wdio-webdriver`, a WebDriver server on 127.0.0.1:4445 that drives the real WKWebView, and is only in effect on the fixtures, so even a test build never opens a real mailbox to it.
The specs take about 15 s; the build before them adds under a minute after a change and several minutes from cold.
Each run gets a scratch directory `/var/tmp/mp-e2e-run-*`, which the app sees as its home, temp, data and config directories (the fixture's draft files included), and WebdriverIO's logs go to its `logs/`.

The app stays out of the way of whoever works on the Mac ([src-tauri/src/e2e.rs](src-tauri/src/e2e.rs)): it runs as an accessory with no Dock icon or menu bar and is never activated, so it never takes the focus or comes to the front.
Its window is on screen, since WebKit pauses the page of a hidden or off-screen window, but fully transparent, click-through, behind every other window and left out of Mission Control.

The specs cover the start on the fixtures, the reader, the draft flows `fixture_simulate` drives (editor save, invalid frontmatter, held, failed, partly delivered and copy-owed sends), and the PERSO-83 compose checks a webview reaches (`cf` with the Signature select, `cn` with an inline Body, `ta` with a typed path).
What they cannot see:

- The reader's HTML frame: it is sandboxed without `allow-same-origin` ([docs/reader.md](docs/reader.md)), so WebDriver, whose scripts run in the app's page, sees only that it loaded; the body is read in Text mode.
- Anything native: the App menu, the file and folder pickers behind Browse and Save, notifications, the terminal window an editor opens, the title bar, and keys that macOS handles before the page (WebDriver's keys are synthesized in the page).
- The installed app: the DMG, its allowance under Privacy & Security, the bundled `mp` and a real daemon, which needs a whole desktop rather than a webview; those checks run in a macOS VM (next section).

The feature never ships: [src-tauri/src/lib.rs](src-tauri/src/lib.rs) refuses it in a release build with `compile_error!`, and `pnpm bundle` fails when the app it built carries the WebDriver server (`node scripts/check-bundle.ts --app <app>`).
The plugin has no commands and no permissions, so [src-tauri/capabilities/default.json](src-tauri/capabilities/default.json) grants it nothing.

## Whole-desktop checks in a macOS VM

What the specs cannot see runs in a Lume macOS 26 VM driven by Cua Driver, on the Mac only (Apple Virtualization.framework).
Lume (`~/.local/bin/lume`, installed with `--no-background-service`) keeps the VMs under `~/.lume`, where APFS clones make a copy cost only its changes; its image cache is off (`lume config caching`), so a pull writes straight into the VM and nothing duplicates the 23 GB image, and a new base VM downloads it again.

No VM is kept between checks since 2026-11-11, to save about 30 GB of disk: a check starts by pulling the image and building the three VMs below, and ends with `lume delete` on each.

| VM | State |
| --- | --- |
| `mp-tahoe-base` | `ghcr.io/trycua/macos-tahoe-cua:latest` as pulled, user and password `lume` |
| `mp-tahoe-golden` | the base plus an SSH key (`~/.lume/ssh/id_ed25519`), Cua Driver in `/Applications`, its Accessibility and Screen Recording grants and its screen-capture consent, and mailypoppins v0.12.0 installed |
| `mp-tahoe-work` | a clone of the golden one to run checks in; `lume delete` it and clone the golden one again for a clean start |

```sh
tuios new-window -s bg lume-vm --cwd /var/tmp --no-focus --keep-open -- lume run mp-tahoe-work --no-display
scripts/vm.sh ssh 'sw_vers -productVersion'         # once `lume get mp-tahoe-work` shows sshAvailable
scp -i ~/.lume/ssh/id_ed25519 <app.dmg> lume@<ip>:Downloads/
scripts/vm.sh shot 01-start                         # guest screenshot to .sync/gui-vm/01-start.png
echo '{"script":"..."}' | scripts/vm.sh call run_script   # Cua Driver: accessibility tree, clicks, typing
scripts/vm.sh vnc move 570 440 click 1              # real input through the VM's VNC port
lume stop mp-tahoe-work
```

The VM runs with no window, so nothing comes to the front on the Mac; VNC listens on 127.0.0.1 only.
Cua Driver acts in the guest's background through the accessibility tree, which reaches the app's webview and its native windows; `--screenshot-out-file` on a driver call saves what it saw.
macOS's secure dialogs ignore its synthetic events, so the admin password prompt takes VNC input, which the guest treats as a keyboard and a mouse.

What the VM has checked (PERSO-83, with the v0.12.0 release DMG): the Gatekeeper block on first open and Open Anyway under Privacy & Security, first launch with its bundled daemon and the setup wizard, the version-mismatch screen against a running v0.11.0 daemon and its Restart daemon, and the native pickers behind Browse (`ta`) and Save.
The password prompt after Open Anyway refuses the right password in this image (the keybag disagrees with the login password, `MKB password verification ... returned -13` in the guest's log), so the check stops there and `xattr -dr com.apple.quarantine` lets the app open.

## Install from a local build

```sh
cd clients/desktop
pnpm bundle                               # builds mp in release, bundles it as the sidecar, runs tauri build
# MP_SIDECAR_BIN=/path/to/mp pnpm bundle  # instead: bundle an mp you already built
```

[scripts/install-local.sh](../../scripts/install-local.sh) at the repository root does the pull, `cargo install`, this bundle, the copy into `/Applications` and the daemon restart in one run (`--no-gui`, `--dry-run`, `-h`).

`pnpm bundle` ([scripts/bundle.ts](scripts/bundle.ts)) copies `mp` to `src-tauri/binaries/mp-<target>` and builds with [src-tauri/tauri.bundle.conf.json](src-tauri/tauri.bundle.conf.json), which names it in `bundle.externalBin`; plain `pnpm tauri build` makes an app without its own `mp`.
It then fails if the app carries the end-to-end tests' WebDriver server, which only the `e2e` feature compiles in.
The app lands in `<target dir>/<target>/release/bundle/macos/mailypoppins.app` and the DMG beside it in `dmg/`, the target dir being `$CARGO_TARGET_DIR` or `src-tauri/target`.
The bundle is unsigned until #0012, so a downloaded copy needs an allowance in System Settings > Privacy & Security ("Open Anyway" after the first blocked launch), or `xattr -dr com.apple.quarantine` on the app; a local build is not quarantined.

```sh
cp -R <target dir>/aarch64-apple-darwin/release/bundle/macos/mailypoppins.app /Applications/
open /Applications/mailypoppins.app
ln -s /Applications/mailypoppins.app/Contents/MacOS/mp /usr/local/bin/mp   # optional: the app's mp on PATH
```

The app starts its daemon with its own `Contents/MacOS/mp` unless `MP_DESKTOP_MP_BIN` names another, and refuses a running daemon of another version with the restart screen, whose Restart runs `mp daemon restart` with that binary.
The symlink keeps the CLI, the TUI and the app's daemon on one version; with a Homebrew or `cargo` `mp` earlier on `PATH`, the two versions meet at the restart screen.
To update: an installed app checks GitHub Releases once a day and offers "Update to <version>" at the sidebar's foot, and "Check for updates" (palette, App menu, Settings) checks at once; the download replaces the bundle in place and "Restart now" switches to it, while after "Later" the next launch restarts the old daemon without the restart screen ([docs/shell.md](docs/shell.md), "Updates").
To uninstall: `mp daemon stop`, quit the app, delete `/Applications/mailypoppins.app` and the symlink; the mail store and `config.toml` are the CLI's and stay (`mp config path`).
[docs/release-process.md](../../docs/release-process.md), "The desktop app", has the release side.

## Environment

| Variable | Effect |
|---|---|
| `MP_DESKTOP_FIXTURE=1` (or `--fixture`) | Serve the fixtures, no daemon |
| `MP_DESKTOP_MP_BIN` | The `mp` binary that starts the daemon and whose version the daemon must run; else the one next to the executable (the bundled sidecar), then `PATH`, then `~/.cargo/bin`, `/opt/homebrew/bin`, `/usr/local/bin` |
| `MP_DESKTOP_WINDOW_SIZE=WxH` | The initial window size, e.g. `950x800` for the medium layout or `600x820` for the narrow one |
| `MP_DESKTOP_STUB_OPENER=1` | "Open in browser" records the URL in the intercepted-URL log instead of opening it, and a file open only logs; automated runs set it |
| `MP_DESKTOP_EDITOR` | The editor command for drafts and every other file the app opens in an editor, `{path}` standing for the file; else the `editor` key of `desktop.json`, `$VISUAL` or `$EDITOR`, a probed `code`, `zed`, `subl` or `cursor`, then `open -t`; a terminal editor runs drafts in the reader pane and opens every other file in a terminal window |
| `MP_DESKTOP_LOG` | `error` to `trace`, default `info`, to stderr and `<data>/logs/mp-desktop.log` |

The daemon's own variables (`MAILYPOPPINS_DATA_DIR`, `MAILYPOPPINS_CONFIG_DIR`, `MAILYPOPPINS_DAEMON_AUTOSTART`) apply as they do to `mp`.

## Documentation

- [docs/rust-layer.md](docs/rust-layer.md): the Tauri commands, the generated TypeScript types, the event stream, the reader scheme, links, the app CSP and fixture mode.
- [docs/shell.md](docs/shell.md): the frontend modules, the model, mutations and pending state, the dialogs and the activity area, compose and send, the outbox, the views, contacts, the calendar, signatures, the activity log, settings, updates, the account wizard, first run, the layouts, focus order and keys.
- [docs/reader.md](docs/reader.md): the reader frame, the toolbar and its Copy menu, invitations, the attachments, the browser rendition, the draft preview and server-only hits, refused links, and the navigation guard's verification with its manual steps.
- [docs/design-tokens.md](docs/design-tokens.md): the semantic tokens and their contrast table.
- [docs/browser-preview.md](docs/browser-preview.md): the frontend in a browser on the fixture backend, for checks before a real window.
