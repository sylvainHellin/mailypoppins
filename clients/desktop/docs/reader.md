# The reader frame and its navigation guard

The reader shows a message body as its own document: an `<iframe>` on `MessageMeta.html_url` (`mpmsg://localhost/<account>/<row_id>`), which the Rust layer's scheme handler answers ([rust-layer.md](rust-layer.md), "The reader").
The rendering rule and its reasons are in the plan, `docs/plans/native-gui.md`, "Reading HTML bodies".

## The frame

`src/components/reader/ReaderBody.tsx` renders:

```html
<iframe src="mpmsg://localhost/work/1006" sandbox="allow-popups" referrerpolicy="no-referrer" title="Message body: …">
```

- `sandbox="allow-popups"` and nothing else: no scripts, no same origin, no forms, no top navigation; `allow-popups` is there only so that a `target=_blank` click reaches `on_new_window`, which refuses it.
- The URL is used as the Rust layer hands it over, so a Windows spelling (`http://mpmsg.localhost/…`) needs no frontend change.
- A skeleton covers the frame until its `load` event; the frame is keyed on the URL, so every message starts with the skeleton.
- The frame draws on `--reader-canvas` (white), because mail is authored for a white page and a message that sets no background would put default black text on the dark shell.
- A message without markup comes back on the same URL as a plain-text document (`X-Mp-Rendition: text`), so React has one path and no longer fetches `message_text`.
- Remote content stays blocked by the reader CSP; M1 has no "load remote images" toggle.
- The body scrolls inside the frame; the header block above it is unchanged.

## Refused links

Every navigation the webview refuses arrives as a `link_intercepted` GuiEvent and lands in `state.intercepted` (the last 100).
One from a navigation or a new window also becomes `state.interceptNotice`, shown in the reader footer (`InterceptedLinkNotice.tsx`): the URL truncated, in full on hover (`title`), with "Open in browser", "Copy" and "Dismiss".
"Open in browser" is the only caller of `open_external`, and only for http, https and mailto; the button is disabled for any other scheme.
The stub's own log line (`source: "open_external_stub"`) raises no notice.
The palette action "Show intercepted links" opens `InterceptedLinksDialog.tsx`, which calls `intercepted_urls()`; that command drains the Rust log, so its answer is merged into `state.intercepted` and the dialog lists the model's copy, newest first.

## Guard verification, 2026-09-30

Run: macOS 26.6.2, WKWebView, `MP_DESKTOP_STUB_OPENER=1 MP_DESKTOP_FIXTURE=1 pnpm tauri dev` in a herdr tab, fixture row 1006 (the hostile message).

Mouse and keyboard input into the app could not be automated.
`cliclick` is not installed, and `AXIsProcessTrusted()` is false for the terminal the agent runs in, so neither `osascript`/System Events nor posted `CGEvent`s reach the window.
Page JavaScript cannot click inside the frame either: the frame is cross-origin to the app and runs no script of its own, so a click inside it needs a real input event.

What was automated instead is a throwaway probe, injected by a Vite config outside the repository (`transformIndexHtml` adds one module script; nothing in `src/` changed).
It selects row 1006 by dispatching `j` keydown events to the app's keymap, waits for the frame's `load`, then acts from the app document.
It reports through the Rust log: each step creates a hidden subframe on `https://probe.invalid/<step>?…`, which `on_navigation` refuses and logs as `[nav] refused Navigation: …`, so the log is an ordered transcript.

| Case | What was exercised | Mechanical | Outcome |
|---|---|---|---|
| (a) plain `<a href=https://…>` click | Not a click. The app document set the reader frame's `src` to `https://evil.example/?from=probe-parent-nav`, which is the same subframe navigation a click in the frame produces | Partly: the navigation, not the click | Refused by `on_navigation`, logged, sent as `link_intercepted`; the footer notice showed the URL; what the frame displayed afterwards was not read back |
| (b) `target=_blank` click | Not a click. `window.open("https://evil.example/?from=probe-window-open")` from the app document, without a user gesture | Partly: `on_new_window`, not the frame's popup path | Refused by `on_new_window` (`source: new_window`), `window.open` returned `null`, no window opened |
| (c) `<form>` submit | Nothing: a submit needs a click or Enter inside the frame | No | Not verified in a webview; `form-action 'none'` in the header CSP and the missing `allow-forms` are the defences, pinned by the unit tests only |
| (d) `<meta http-equiv=refresh content=0>` | The real message loaded in the real frame; the probe waited 4 s after `load` | Yes | No `from=meta-refresh` refusal in the log, so no navigation reached `on_navigation`; the fixture keeps its meta refresh on purpose |
| (e) remote `<img>` | The real message loaded | No | Not observed: no network capture without root, and a blocked image leaves no trace in our log. The header CSP (`img-src data:`) is pinned by `reader.rs`'s tests |

Also observed in the same run:

- The frame loaded `mpmsg://localhost/work/1006` and fired `load`, with `sandbox="allow-popups"` and `referrerpolicy="no-referrer"` read back from the live DOM.
- No `from=script`, `from=onload` or `from=fetch` refusal appeared, so neither the inline scripts nor the `onload` `window.open` ran; the message's own `<iframe src=https://evil.example/frame>` produced no refusal either, so the message CSP stopped it before `on_navigation`.
- `intercepted_urls()` returned the 7 entries of the run, in order, the probe's markers included.
- The main document stayed on `http://localhost:1420/` (the dev origin) throughout.
- No `[open] stubbed` line appeared: nothing called `open_external`.

Still open for a person at the Mac: click (a) the plain link, (b) the `target=_blank` link and (c) the Verify button in row 1006, and check that each lands in the footer notice or, for the form, that nothing happens; then click "Open in browser" once and check for exactly one `[open] stubbed` line.
