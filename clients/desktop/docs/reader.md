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

## The toolbar

The open message's toolbar (`ReaderToolbar.tsx`, `role="toolbar"`, "Message actions") runs the same actions as the keys, on this message only, whatever the list has marked.
Reply and Reply all write the reply with `draft_reply` and open it in the editor, Forward opens the forward wizard, and Archive, Delete, Move, Flag and Mark read or unread follow ([shell.md](shell.md), "Compose" and "Actions, dialogs and the activity area").
Open in browser (`tb`) is last.

## Attachments

The header block lists the message's attachments from `MessageMeta.attachments`, each with its name, its size, and two icon buttons named after the file: "Open <name>" (`to`) and "Save <name>" (`ts`).
Open calls `attachment_open`, which hands the daemon's copy of the part to the system opener, and the status line says "Opened: <name>", the TUI's words ([rust-layer.md](rust-layer.md), "Attachments").
Save opens the Save dialog for that one part.

The keys act on the cursor message from the list or the reader, as the TUI's MESSAGE keys do:

- `to` opens a message's only attachment at once, and over several opens the "Open attachment" dialog, one button per part, the first focused.
- `ts` opens the Save dialog with every part checked.
- A message with none says "No attachments", the TUI's line.

The Save dialog ("Save attachment", or "Save attachments" over several parts) has a checkbox per part and a Directory field, which stands in for the TUI's directory picker and the native folder picker until the dialog plugin is installed.
The field starts at `~/Downloads`, the TUI's default, and then at the last directory a save went to in this window.
It takes an absolute path or one starting with `~`; a refusal, such as a relative path, shows in the dialog's alert, which keeps it open.
A save that went through closes the dialog and says "Saved 2 files to ~/Downloads" in the activity area, with the directory as typed; parts that failed turn it into an alert naming why.

## The browser rendition

`tb`, the toolbar's Open in browser, or the palette's "Open HTML in browser" hands the message to the default browser through `html_open`: the daemon's rendition, with the charset, the CSP tag and the `cid:` images inlined.
The status line says "Opened in browser", or "No HTML version available" for a message whose sender wrote no markup.
On a server-only hit, `tb` writes the hit's own markup into the app cache through `hit_html_open`, and a hit without markup gets the same line; a draft has no rendition.

## Drafts and server-only hits

The reader pane shows no frame for these two.

A selected draft shows `DraftPreview.tsx` (`src/components/compose/`): the `draft_preview` record, which is the headers (From, To, Cc, Bcc), the status pill, the body as the dry run cuts it at 500 characters, and the file path.
Under the headers, `draft_validate`'s report says "Valid" or "Not sendable" with the error, and lists the warnings.
Under the report, the "Attachments" list shows each entry of the draft's `attachments:` as the file spells it, from `draft_attachments`, with "Open <entry>" and "Remove <entry>" buttons.
An entry with no file behind it carries a "missing" badge and its Open is disabled, since the send would fail on it.
Remove calls `draft_attachment_remove`, which takes the entry out of the file and leaves the file it named alone.
The three are read again whenever the listing's row or the list itself changes, which is what a save in the editor or an attach does through `draft.changed`.
Its toolbar, "Draft actions", has Edit in editor (`e`), Edit recipients (`ce`), Attach file (`ta`), and Approve (`cA`) or Back to draft (`cD`).
Approve and Back to draft act on the draft the preview shows and ask nothing, even with drafts marked; a draft being sent is refused with a notice, as by the keys.

Attach file, `ta` in Drafts, opens the "Attach file" dialog, whose File field takes an absolute path or one starting with `~`, stored in the draft as typed.
The native file picker arrives with the dialog plugin, and the dialog says so under the field.
A path with no file behind it ("No such file: ~/nope.pdf", the TUI's words), a relative path, a directory, and a file the draft already lists are refused in the dialog's alert, which keeps it open for a correction, as the TUI's prompt stays armed.
An attach that went through closes the dialog and says "Attached <path> to <draft>".
On a draft, `to` opens one of its files, or picks among several in the same dialog as a message's parts, and `ts` says the files are already on disk.
A file that does not parse gets no preview call: the pane says "This draft does not parse" with the listing's diagnostic, and only Edit in editor stays, since the editor is where it gets fixed.

A server-only search hit has no row, so there is no body to load.
The pane shows its sender, date and mailbox, says the message is on the server only, and offers Reply, Reply all and Forward, which `draft_from_message` builds from the hit's own headers, with no attachments.
Its Fetch button, or `F`, is the TUI search overlay's `f`: `message_fetch` downloads the message into the store, the button says "Fetching…" meanwhile, and the hit then becomes the row it landed in, so the reader loads it like any stored message and the activity area says "Fetched into the local store".
`F` on a hit the store already holds says "Already in the local store", and a refused fetch is an alert naming the daemon's reason.
`to` and `ts` on a server-only hit say to fetch it first, since only a stored message has parts to materialise; Open in browser shows when the hit carries markup.

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
| (a) plain `<a href=https://…>` click | The probe set the reader frame's `src` to `https://evil.example/?from=probe-parent-nav`; the owner then clicked the link itself on 2026-09-30 | Partly, then by hand | Refused by `on_navigation`, logged, sent as `link_intercepted`; the footer notice showed `https://evil.example/?from=plain-link`; the message stayed in the frame |
| (b) `target=_blank` click | The probe called `window.open` from the app document; the owner then clicked the link itself on 2026-09-30 | Partly, then by hand | Refused by `on_new_window`, no window opened; the footer notice showed `https://evil.example/?from=target-blank` |
| (c) `<form>` submit | The owner typed into the password field and clicked "Verify" on 2026-09-30 | By hand | Nothing happened: no notice, no navigation, the message stayed in the frame; `form-action 'none'` in the header CSP and the missing `allow-forms` hold |
| (d) `<meta http-equiv=refresh content=0>` | The real message loaded in the real frame; the probe waited 4 s after `load` | Yes | No `from=meta-refresh` refusal in the log, so no navigation reached `on_navigation`; the fixture keeps its meta refresh on purpose |
| (e) remote `<img>` | The real message loaded | No | Not observed: no network capture without root, and a blocked image leaves no trace in our log. The header CSP (`img-src data:`) is pinned by `reader.rs`'s tests |

Also observed in the same run:

- The frame loaded `mpmsg://localhost/work/1006` and fired `load`, with `sandbox="allow-popups"` and `referrerpolicy="no-referrer"` read back from the live DOM.
- No `from=script`, `from=onload` or `from=fetch` refusal appeared, so neither the inline scripts nor the `onload` `window.open` ran; the message's own `<iframe src=https://evil.example/frame>` produced no refusal either, so the message CSP stopped it before `on_navigation`.
- `intercepted_urls()` returned the 7 entries of the run, in order, the probe's markers included.
- The main document stayed on `http://localhost:1420/` (the dev origin) throughout.
- No `[open] stubbed` line appeared: nothing called `open_external`.

## Known limitation

Once focus is inside the reader frame (a click in the message body), the app's keys no longer reach the app: the frame is cross-origin to it and runs no script, so its key events stay in its own document and the keymap never hears them.
Escape, `j`/`k`, `:` and every other app key do nothing until focus returns to the app, by a click on the header, the list or the sidebar.
`j`/`k` on the reader pane scroll `#mp-reader-scroll`, which holds the header block and the frame; the body scrolls inside the frame, so from the keyboard only the header area moves.

## Manual verification

Cases (a), (b) and (c) of the table above need a real click, which the agent cannot post (see the run above); the owner ran these steps on 2026-09-30 and all three held, including step 6, whose stub line replaces the browser.
Repeat them after any change to the scheme handler, the navigation hooks or the frame's `sandbox`.

1. In a herdr tab: `cd clients/desktop && MP_DESKTOP_STUB_OPENER=1 MP_DESKTOP_FIXTURE=1 pnpm tauri dev`, and keep its output in view (it also goes to `<data>/logs/mp-desktop.log`).
2. In the Inbox of `work`, click row 1006, "Action required: verify your account" from Security Team, the sixth row, and wait for the body.
3. Case (a): click "plain https link".
   Expected: the footer shows the "Blocked link" notice, "Link blocked: https://evil.example/?from=plain-link"; the log gains `[nav] refused Navigation: https://evil.example/?from=plain-link`; the frame still shows the message; no `[open] stubbed` line appears.
   Click "Dismiss".
4. Case (b): click "target=_blank link".
   Expected: the notice shows `https://evil.example/?from=target-blank`; the log gains `[nav] refused NewWindow: https://evil.example/?from=target-blank`; no window opens; no `[open] stubbed` line appears.
   Click "Dismiss".
5. Case (c): type anything in the password field and click "Verify".
   Expected: nothing happens; no notice, no `[nav] refused` line for `https://evil.example/collect`, no `[open] stubbed` line; the frame still shows the message.
6. Last, show the notice again with step 3 and click "Open in browser" once.
   Expected: exactly one `[open] stubbed: https://evil.example/?from=plain-link` line and no browser window; the palette's "Show intercepted links" lists it as "opener stub".

The verdict of each step goes into the table above, with the date of the run.
