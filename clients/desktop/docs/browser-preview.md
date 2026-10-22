# Browser preview

The browser preview runs the frontend in an ordinary browser, with every Tauri command answered by the fixture backend the tests use.
It lets an agent with a browser, or Sylvain without a build, check the screens, the keys and the mouse paths of a change before a real window does.
What only the window can show stays for `MP_DESKTOP_FIXTURE=1 pnpm tauri dev` or the installed app: the native menu, the file and folder pickers, the embedded terminal, the updater, the reader's `mpmsg` scheme and its navigation guard, and every behaviour of a real daemon.

## Run

```sh
cd clients/desktop
pnpm preview:browser                          # http://localhost:1430
pnpm preview:browser --host 100.76.10.100     # on the Mac's tailnet address, for BrowserOS neo on the home server
```

The dev server listens on port 1430, beside `pnpm tauri dev` on 1420.
A reload starts from the fixtures again, with the preferences the page kept in its `localStorage`.

## How it is wired

`src/fixture/backend.ts` is the fixture backend: plain TypeScript that answers every command from `fixtures/*.json` the way `src-tauri/src/fixture.rs` does, holds the event Channel, and keeps its state in `mock`.
`src/test/tauri-mock.ts` is the vitest face of it, the entry points wrapped in `vi.fn`.
`src/fixture/preview.ts` is the preview's face: vite.config.ts aliases `@/lib/tauri` to it in the `browser` mode only.
The reader frame loads a blob of what the backend serves for the `mpmsg` URL, under the reader CSP and with the bridge, both read from `src-tauri/src/reader.rs`.

Three guards keep the backend out of the app:

- the alias exists only in the `browser` mode, and vite.config.ts refuses `vite build` in that mode;
- `scripts/fixture-guard.ts`, a build-only plugin in vite.config.ts, fails `vite build` when any module under `src/fixture/` is loaded, an import of a single export included;
- `pnpm build`, which `tauri build` runs, ends with `scripts/check-bundle.ts`, which fails when `dist/` holds a string only the backend carries, a backstop for what the plugin misses.

## Driving the fixture

`window.mpFixture` is the backend module, for the browser console or an agent's `evaluate`.
The fixture's long operations wait until their driver settles them, as in the tests; fixture.rs settles them on its own timers instead.

```js
mpFixture.mock                                   // the state: rows, drafts, holds, outbox, settings, ...
mpFixture.emitEnvelope("state.invalidate", { resource: "mailbox:work/inbox", scope: { query: "counts" } })
mpFixture.emit({ type: "connection", status: { state: "failed", error: { kind: "version_mismatch", why: "...", socket: "", log: "", daemon_version: "0.9.0" } } })
mpFixture.emitMenu("settings")                   // a native menu item, by its id in src-tauri/src/menu.rs
mpFixture.settleRebuild()                        // the oldest contact rebuild; { saved: "refused_shrunk" } or { fail: "..." }
mpFixture.settleRsvp()                           // the oldest RSVP; { fail: true }
mpFixture.settleInvite()                         // the oldest invitation sent
mpFixture.reportDeviceCode(); mpFixture.settleSignIn()
mpFixture.simulateInvite(false)                  // invite_update; true for invite_cancel
mpFixture.simulateSignatureChanged()
mpFixture.mock.picked = "/home/fixture/Documents/report.pdf"   // what the next Browse answers
```

Holds never fire on their own: Cancel ends one, and `mpFixture.mock.holds` lists them.
An editor open is journaled in `mpFixture.mock.editorOpens` and launches nothing, as fixture mode does.
