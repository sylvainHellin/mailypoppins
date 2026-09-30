# M0 GUI spike results (#0128)

The measurements and findings of the M0 spike of the [native GUI plan](../plans/native-gui.md), ticket [#0128](../tickets/0128-gui-risk-spikes.md).
Every later GUI milestone compares against these numbers rather than against remembered ones.

Taken on 2026-09-30 on branch `origin/gui-spike`, commits `a4a5be7` to `5768543` on top of the scaffold `0332a44`.
The host was macOS 26.6.2 on Apple Silicon, and every number comes from the release build unless its line says dev.
The spike code lives in `spikes/gui-neovim/` on that branch only, and file paths below are relative to that directory.
The branch is kept as reference for M1 and is deleted when M1 lands, so this file is the lasting record.

The text below is the spike's `RESULTS.md` as committed in `5768543`.

Measured on 2026-09-30 on Sylvain's MacBook Pro (Apple Silicon, macOS 26.6.2, build 25G83), against the running daemon (mp 0.10.0, accounts `tum` and `proton`, real mail).
The task text says macOS 15; this machine runs macOS 26, so every WebKit finding below is for macOS 26.
Stack: Tauri 2.12.0, wry 0.57.0, tauri-plugin-opener 2.7.0, portable-pty 0.9.0, @xterm/xterm 6 with the WebGL addon (it loaded; no fallback to the DOM renderer), React 19, Tailwind 4.
Neovim is the user's own (`~/.local/share/bob/nvim-bin/nvim`, found on `PATH`) with the user's config and plugins.

## How the numbers were taken

- `MP_SPIKE_AUTO=<mode>` runs a scripted scenario in the webview (`src/auto.ts`).
  Every `[metric]` line is printed by the Rust side, stamped `t=` in ms since `main` started (`Instant`).
- Unless marked dev, numbers come from the release binary (`pnpm tauri build --no-bundle`, then `src-tauri/target/release/gui-neovim`), because `tauri dev` does not apply the app CSP (it loads `devUrl` directly) and runs unoptimised Rust.
- WebKit clamps `performance.now()` to 1 ms, so sub-millisecond values read as 0 or 1.
- Memory is `footprint` (physical footprint, what Activity Monitor shows) summed over the app process and the three WebKit XPC processes started with it (WebContent, GPU, Networking).
  RSS sums are given too, but they double-count shared WebKit pages.
- Raw logs: `/var/tmp/mp-spike-{rel1,rel2,final,nvim,csp,clickA,clickB,dev-auto,devlinks}.log` and the matching `*-mem.txt`.
- Synthetic mouse clicks were impossible: neither `osascript` nor a CGEvent poster has Accessibility permission (`AXIsProcessTrusted() = false`).
  Link clicks were therefore probed with a script-driven `.click()` in a test document (see "Links").

## Numbers against the plan's performance targets

### Cold start to a painted list: target under 1 s cold, under 500 ms warm

- Release, first launch after the build: list painted at 531 ms after `main`.
- Release, five back-to-back launches: 423, 357, 364, 384, 355 ms (median 364 ms).
- Wall time from `exec` to the log line, including shell polling: 434 to 590 ms.
- Breakdown of one run (final): Tauri `setup` at 61 ms, daemon session connected at 99 ms (handshake 38 ms, off the UI path), first React effect at 415 ms, `state.bootstrap` 0.8 ms in Rust and 17 ms round trip from JS, `message.list` of 10 rows plus paint 31 ms, painted at 464 ms.
- Dev (`tauri dev`, Vite serving modules): 515 to 793 ms.
- Verdict: both targets met; about 300 ms of the budget is WKWebView creation and page load before the first React effect, which Tauri controls, not the app.
- A true cold start (empty page cache after a reboot) was not measured.

### List keyboard navigation: target one frame, 16 ms

- 50 synthetic `j`/`k` presses on a 500-row list (tum/archive): keydown to the next animation frame median 0 ms, p90 1 to 2 ms, max 2 ms.
- Real key presses Sylvain made in the dev build: 2 to 5 ms.
- It feels instant; cursor and scroll follow without visible lag.
- The metric ends at the frame callback, before the compositor presents, so it misses the present latency.
- Verdict: met.

### Opening a message: target plain text under 100 ms, HTML under 250 ms

- Survey of the first 40 rows of tum/archive: 38 HTML, 2 plain text; every HTML open landed between 9 and 36 ms.
- Plain text (`message.get` into a `<pre>`): 9 to 27 ms.
- HTML, small message (1,150 bytes), srcdoc route: median 5 ms, p90 8 ms.
- HTML, large message (320,053 bytes), srcdoc route: median 19 ms, p90 22 ms; `message.html` itself 4 to 22 ms of that.
- Same messages over the `mpmsg://` custom scheme: small median 4 ms, large median 13 ms, p90 14 to 25 ms.
- Measured from the open call to the iframe `load` event, over five alternating opens after a first pass (warm webview).
- Verdict: met by an order of magnitude on both routes.
- The `-32004` fallback (`message.materialise_html`, read the file, `message.release_handle`) is implemented but was not exercised: no message in these accounts exceeds 8 MiB.

### Memory with one account open and the reader showing HTML: target under 300 MB

- Footprint with the 320 KB HTML message open: 161 MB and 181 MB in two runs (app 34 MB, WebContent 96 to 102 MB, GPU 25 to 40 MB, Networking 6 MB).
- Over a whole scenario the sum ranged from 154 to 268 MB; the peaks are the GPU process while iframes are swapped.
- After the 1.5 MB terminal burst: 214 MB footprint, but WebContent RSS rose to about 700 MB (the probe keeps 100k chunks alive, plus xterm's 10k-line scrollback).
- RSS sum with HTML showing: 368 to 393 MB, which overstates because of shared WebKit pages.
- Verdict: met on footprint; RSS would miss it, so the plan should name footprint as the metric.

### Neovim in the terminal: target cold start under 300 ms, keystroke echo under 30 ms

- PTY spawn to first output: 14 to 59 ms.
- Spawn to the statusline (file name) parsed into xterm's buffer: 219 ms with the window focused, 301 ms in a run where the window had lost focus.
- Reference: `nvim --headless --startuptime` with the same config reports 137 ms to `NVIM STARTED`.
- Echo in insert mode, 43 keys through `term.input()` (the same `onData` path a keydown takes): keypress to PTY data on the channel median 3 ms, p90 5 ms, max 15 ms; keypress to xterm render median 4 ms, p90 7 ms, max 18 ms.
- Verdict: both met on this config; start is close to the line (219 ms of 300), and a heavier plugin set would cross it.

## Findings

### Daemon connection

- `mp_client::session::Session` works as is from a Tauri app: a `Connector` of two plain `fn`s, `ClientKind::Gui`, the CLI's socket path (`<data_dir>/runtime/daemon.sock`) and identity (`data_dir`, `config_dir`), derived here by hand from `MAILYPOPPINS_DATA_DIR` / `MAILYPOPPINS_CONFIG_DIR` or the macOS defaults.
- The session connects on a background thread so the window paints first; commands wait on a `Condvar` until the `QueryHandle` exists.
- The event stream is drained and discarded; the spike does not apply events.
- No auto-start: the spike's `open` retries every 500 ms until a daemon answers.

### HTML reader

- Inline `data:` images render in the srcdoc route only when the app CSP allows them: with `img-src 'self'` the SVG and PNG test images disappear (screenshot 5); with `img-src 'self' data:` they render (screenshot 3).
  The srcdoc document inherits the app CSP, which confirms the research report.
- The custom-scheme route renders `data:` images whatever the app CSP says (screenshot 6), because the document carries its own `Content-Security-Policy` response header.
- Remote images are blocked on both routes, by the message's own CSP.
- Inline `<style>` in the message applies on both routes (the app CSP has `style-src 'unsafe-inline'`).
- The `mpmsg://localhost/<account>/<row_id>` iframe loads and renders on macOS 26 and fires `load`; bug #12767 did not reproduce.
- The iframe needs `frame-src mpmsg:` in the app CSP; `http://mpmsg.localhost` is listed for Windows parity and untested.
- A `<meta http-equiv="refresh">` to an https URL does nothing in `sandbox=""` frames on either route: no navigation reaches `on_navigation` (the sandbox's automatic-features flag).
- The app CSP set in `tauri.conf.json`: `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; frame-src mpmsg: http://mpmsg.localhost https: http:; connect-src ipc: http://ipc.localhost`.

### Links

- `on_navigation` on macOS sees subframe navigations, not only the main frame: every srcdoc load arrives as `about:srcdoc` and every scheme load as `mpmsg://localhost/...`, so the allowlist must admit both.
- With `frame-src` limited to `mpmsg:`, a link clicked in the message frame never reaches `on_navigation`: the app CSP blocks the frame navigation first, so the link is silently dead and nothing opens in the browser.
- With `frame-src` admitting `https: http:`, a plain link click reaches `on_navigation`, which denies it and hands the URL on for the browser (`[nav] DENY https://example.com/?from=plain-link`).
  The frame stays on the message.
  `on_navigation` is then the only barrier for http(s) frame loads.
- `target=_blank` in a frame without `allow-popups` is dropped by the sandbox: nothing reaches Rust and nothing opens.
- With `sandbox="allow-scripts allow-popups"`, a `target=_blank` click reaches `on_navigation` (not `on_new_window`) and is denied.
- `window.open()` from the app document reaches `on_new_window` (`NewWindowResponse::Deny`); the report's "unverified" for new-window interception is resolved, the hook exists in Tauri 2.12.
- Opening in the browser is stubbed: both hooks log `[intercepted] ... would open in browser (stubbed): <url>` to stderr and append the URL to a list the header shows (`intercepted (not opened): ...`), and nothing calls the opener plugin.
  Early runs did call `opener().open_url()` and opened example.com tabs in Sylvain's browser, which showed the plugin works from both hooks; that call was removed because Sylvain works on this machine.
  The opener plugin stays registered, so M1 only has to restore the call.
- The click probes used a test document served over `mpmsg` with `allow-scripts` and a script calling `.click()`; a real user click on a real message is still unverified.
  To check by hand: run `pnpm tauri dev` or the release binary, press "test doc", click each link.
- While the opener was live, each probe's browser tab stole focus from the app, which is what occluded the window in the final run below.

### PTY and xterm.js

- `portable-pty` spawns the user's Neovim with `PATH`, `TERM=xterm-256color` and `COLORTERM=truecolor`; the config, plugins, colours and statusline load (screenshot 7).
- Output travels as raw bytes on a `Channel<InvokeResponseBody>` (`InvokeResponseBody::Raw`), which JS receives as `ArrayBuffer`; a `Channel<Vec<u8>>` would serialise each chunk as a JSON number array.
- Ordering held: `seq 1 200000` (1.49 MB) arrived as 200,000 lines in order, in both builds, and `cat` of a 149 KB Markdown file rendered without garbling (screenshots 8 and 9).
- One run carried a doubled CR (`547\r\r\n548`) in the raw bytes Rust read from the PTY, so it comes from the macOS tty's ONLCR processing under a full output queue, not from the channel.
- Throughput is the weak point: 7.2 MB/s in dev (3.9k chunks) but 1.2 MB/s in release (106k chunks), because the faster release reader sends many chunks under 1 KiB and Tauri delivers each of those through a separate `webview.eval`.
  Coalescing reads in Rust (for example up to 64 KiB or 4 ms) should fix it; not tried.
- An occluded or unfocused window stops rendering: xterm's `onRender` never fired, timers were throttled, and the 43-key echo run took 125 s instead of 10 s, while PTY data kept arriving (echo to data median 5 ms).
  Latency measurements need the window in front.
- `pty_resize` is wired to xterm's `onResize` and the fit addon, but resize behaviour, clipboard, IME, mouse, Unicode width, keyboard routing between app and terminal, child cleanup on window close or crash, the draft-watcher round trip and a signed-app launch were not tested in this box.

## Open questions for the plan

- Should the app CSP admit `https: http:` in `frame-src` so link clicks reach `on_navigation`, or should the reader rewrite every `<a href>` before rendering and keep `frame-src` closed?
- Should the reader grant `allow-popups` so `target=_blank` links open in the browser instead of doing nothing?
- Which route should the reader use?
  The custom scheme gives a per-message CSP header and ignores the app CSP; srcdoc is simpler but couples message rendering to the app CSP.
- Should the plan name physical footprint as the memory metric, since RSS reads over 300 MB for the same state?
- Neovim start sits at 219 ms of the 300 ms budget with this config; should the target exclude the user's own plugin load time?
- Does the Rust PTY reader need coalescing before M5, given 1.2 MB/s in release?

## Screenshots

In `~/Documents/tmp/`:

- `mp-spike-1-dev-three-panes.png`: accounts, mailboxes and the tum inbox list (dev build).
- `mp-spike-2-rel1-memory-large-html.png`: the 320 KB HTML message in the srcdoc reader.
- `mp-spike-3-rel1-testdoc-srcdoc.png` and `mp-spike-4-rel1-testdoc-scheme.png`: the test document on both routes with `img-src data:` allowed.
- `mp-spike-5-csp-testdoc-srcdoc.png` and `mp-spike-6-csp-testdoc-scheme.png`: the same with `data:` removed from the app CSP; srcdoc loses the images, the scheme route keeps them.
- `mp-spike-7-nvim-nvim-typed.png`: Neovim in xterm.js after the typing probe.
- `mp-spike-8-rel1-burst-seq.png` and `mp-spike-9-rel1-burst-cat.png`: the terminal after the `seq` and `cat` bursts.
- `mp-spike-10-click-probe.png`: the test document after the scripted clicks, still on the message.
