# mailypoppins desktop client

The macOS GUI of mailypoppins, a third client of the daemon beside the CLI and the TUI.
It is built with Tauri 2, React 19, Vite, Tailwind 4 and shadcn on Base UI.
The plan is [docs/plans/native-gui.md](../../docs/plans/native-gui.md), and the tickets are [#0129](../../docs/tickets/0129-gui-shell-and-design-system.md) for M1 and [#0131](../../docs/tickets/0131-gui-full-parity.md) for M2 to M4.

## What works

- M1: accounts, mailboxes with counts and sync health, the message list, the sandboxed reader, and local and server search.
- M2: archive, delete, move, flag and read, on one row or on marked rows, applied at once and put back on a refusal or a rollback, with the TUI's keys, palette entries, row toggles and a reader toolbar.
- M2: the archive and delete confirmation, the move picker, mark read on an explicit open, quick and full sync, and the activity area with the send-hold countdown and Cancel.
- Composing, the outbox view, calendar, contacts and settings come with M3 and M4.

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

Fixture mode serves `fixtures/*.json`: 2 accounts, 6 mailboxes, 21 messages, 2 drafts, 3 HTML bodies and 1 armed send hold, row 1006 being a hostile message.
It is the way to iterate on the screens without a mail server.

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

pnpm tauri build                        # an unsigned app bundle; signing and notarisation are M6 (#0132)
```

## Environment

| Variable | Effect |
|---|---|
| `MP_DESKTOP_FIXTURE=1` (or `--fixture`) | Serve the fixtures, no daemon |
| `MP_DESKTOP_MP_BIN` | The `mp` binary that starts the daemon; else the one next to the executable, then `PATH`, then `~/.cargo/bin`, `/opt/homebrew/bin`, `/usr/local/bin` |
| `MP_DESKTOP_WINDOW_SIZE=WxH` | The initial window size, e.g. `950x800` for the medium layout or `600x820` for the narrow one |
| `MP_DESKTOP_STUB_OPENER=1` | "Open in browser" records the URL in the intercepted-URL log instead of opening it; automated runs set it |
| `MP_DESKTOP_LOG` | `error` to `trace`, default `info`, to stderr and `<data>/logs/mp-desktop.log` |

The daemon's own variables (`MAILYPOPPINS_DATA_DIR`, `MAILYPOPPINS_CONFIG_DIR`, `MAILYPOPPINS_DAEMON_AUTOSTART`) apply as they do to `mp`.

## Documentation

- [docs/rust-layer.md](docs/rust-layer.md): the Tauri commands, the generated TypeScript types, the event stream, the reader scheme, links, the app CSP and fixture mode.
- [docs/shell.md](docs/shell.md): the frontend modules, the model, mutations and pending state, the dialogs and the activity area, the layouts, focus order and keys.
- [docs/reader.md](docs/reader.md): the reader frame, refused links, and the navigation guard's verification with its manual steps.
- [docs/design-tokens.md](docs/design-tokens.md): the semantic tokens and their contrast table.
