# The reader frame and its navigation guard

The reader shows a message body as its own document: an `<iframe>` on `MessageMeta.html_url` (`mpmsg://localhost/<account>/<row_id>`), which the Rust layer's scheme handler answers ([rust-layer.md](rust-layer.md), "The reader").
The rendering rule and its reasons are in the plan, `docs/plans/native-gui.md`, "Reading HTML bodies".

## The frame

`src/components/reader/ReaderBody.tsx` renders:

```html
<iframe data-slot="reader-frame" src="mpmsg://localhost/work/1006" sandbox="allow-popups allow-scripts" referrerpolicy="no-referrer" title="Message body: …">
```

- `sandbox="allow-popups allow-scripts"` and nothing else: no same origin, no forms, no top navigation; `allow-popups` is there only so that a `target=_blank` click reaches `on_new_window`, which refuses it.
- `allow-scripts` runs no sender script: the only script is the app's own bridge (see The bridge), admitted by a nonce the reader CSP carries for that one response.
- Never `allow-same-origin`: with scripts allowed it would give the frame the app's origin, and `reader.test.tsx` pins its absence.
- The URL is used as the Rust layer hands it over, so a Windows spelling (`http://mpmsg.localhost/…`) needs no frontend change.
- A skeleton covers the frame until its `load` event; the frame is keyed on the URL, so every message starts with the skeleton.
- The frame draws on `--reader-canvas` (white) in both palettes, because mail is authored for a white page and a message that sets no background would put default black text on the dark palette's Prussian.
- A message without markup comes back on the same URL as a plain-text document (`X-Mp-Rendition: text`) carrying the bridge too, so in html mode React has one path, fetches no `message_text`, and the scroll keys work on it as on HTML.
- Remote content stays blocked by the reader CSP; M1 has no "load remote images" toggle.
- The body scrolls inside the frame; the header block above it is unchanged, and the reader's scroll keys reach the body through the bridge.

## The bridge

The frame is an opaque origin, so the app can neither scroll its document nor hear its keys; the bridge is the one script that does both (PERSO-81).
Its source is the constant `BRIDGE` in `src-tauri/src/reader.rs`, minified by hand, with no dependency.

- For every HTML or text response the handler draws a fresh nonce, 16 bytes from the operating system's generator in base64 (`fresh_nonce`), and serves the policy `message_csp(nonce)`: the daemon's plus `script-src 'nonce-<n>'`.
- It puts `<script nonce="<n>">BRIDGE</script>` once into the HTML rendition, right after the CSP meta at the start of the document, ahead of any sender markup; it searches for no `<head>`, which a sender can spoof, for the same reason the daemon does not.
- It replaces every copy of the daemon's meta (`default-src 'none'`, which would block the bridge) with one carrying the same policy as the header, nonce included.
- No sender `<script>`, `on*` handler or `javascript:` URL carries a nonce the sender could know when writing the message, so the message runs nothing, and a pasted copy of the bridge does not run either.
- A nonce rather than the bridge's hash: under CSP Level 3 a hash source also admits `<script src=… integrity="sha256-<the same hash>">`, and the bridge's hash is public, so a sender could have made the frame fetch one URL.
- The text rendition, the plain-text document the handler builds for a message without markup, gets the bridge the same way, under its own fresh nonce, with `message_csp(nonce)` in the header and in its meta; it holds no sender markup, since the app writes it from constants and the escaped stored text.
- A rendition without the daemon's meta is served as it came, with no bridge and under the daemon's policy alone, and so is any response for which no nonce could be drawn.
- The browser rendition (`tb`, `html_open`) is the daemon's file and keeps its script-free meta.

The parent posts `{type: "scroll", dy}` and `{type: "scrollTo", y: "top" | "bottom"}` to the frame's window with target `"*"`, the only target an opaque origin matches (`scrollReaderFrame`, `scrollReaderFrameTo` in `ReaderBody.tsx`).
The bridge obeys a message only from `window.parent` and only in those two shapes with a finite `dy`, and scrolls `document.scrollingElement`.
In html mode `j`/`k`, the arrows, `Ctrl+d`/`Ctrl+u` and `PageDown`/`PageUp` scroll the body; `G`, `End`, `gg` and `Home` move the body and the pane holding the header block to the same end.

The bridge posts every keydown inside the frame to the parent as `{type: "key", key, code, ctrlKey, metaKey, altKey, shiftKey}`.
It prevents the key's default unless Ctrl or Cmd is held, so Space, the arrows and the Page keys do not also scroll the frame natively, and copy and select all still work on the message text.
One listener in `useKeymap.ts` takes a key message only when its `source` is the reader frame's window and the frame holds the focus, the only way a real key reaches it (`forwardedKey`), and ignores every other message.
It records the reader as the focused pane, then dispatches the key again as a `keydown` on the frame element, so the keymap runs it through the same handler as a key pressed outside the frame: `Escape`, `:`, `J`, a `g` prefix and the rest behave alike inside and outside the message.
`bridge.test.ts` runs the bridge out of the Rust source in a jsdom frame; jsdom enforces neither the sandbox nor the CSP, which only the manual run below checks.

## Text mode

The reader shows a message in one of two modes, `html` (the frame above, the default) or `text`.
Text mode shows the stored plain text, what the TUI's preview shows, as `ReaderText` in `src/components/reader/ReaderBody.tsx`, with no frame.
It reads `message_text` once per message and reader load, caches the answer in `src/app/readerMode.ts`, and drops an answer whose message is no longer open.
The cache key holds the daemon instance and the `Message-ID` beside the row id, since a restarted daemon may give a row id to another message.
The text sits in a `<pre>` named "Message text: <subject>", on `bg-background` in `text-foreground` and the mono font, so it follows the app's theme.
Line breaks stay as stored, long lines wrap, and a line whose first non-blank character is `>` is `text-muted-foreground`.
The text flows in `#mp-reader-scroll`, so `j`/`k`, `Ctrl+d`/`Ctrl+u`, `PageDown`/`PageUp`, `G`, `gg` and `Home`/`End` scroll the body itself, and `z` zooms the reader as in html mode.
A message whose store holds no text says "No text body" with a hint that `t t` shows the HTML version, and the mode stays text.
A read that fails says "The text did not load: <why>".

`tt` from the list or the reader toggles the mode, and from the sidebar does nothing.
The toolbar's "Reader mode" group holds HTML and Text, the current one pressed, and Settings has the same pair under "Reader".
The palette's READER rows are "Toggle reader text mode" (`tt`), "Reader: HTML" and "Reader: text".
The mode is the `reader_mode` key of `desktop.json` ([rust-layer.md](rust-layer.md), "Desktop settings"), read once at startup into `state.readerMode`, which is html until the read answers.
A change shows at once and is then stored; a refused write keeps the mode for this window and says "The reader mode was not saved: <why>".
`tb` still opens the HTML in the browser from text mode.
The TUI's `tt` is its thread view, which the desktop does not have yet; the KEYMAP row "Show conversation (thread)" stays badged and lists no key.

## The toolbar

The open message's toolbar (`ReaderToolbar.tsx`, `role="toolbar"`, "Message actions") runs the same actions as the keys, on this message only, whatever the list has marked.
Reply and Reply all write the reply with `draft_reply` and open it in the editor, Forward opens the forward wizard, and Archive, Delete, Move, Flag and Mark read or unread follow ([shell.md](shell.md), "Compose" and "Actions, dialogs and the activity area").
Open in browser (`tb`) comes next, then the reader mode's HTML and Text (`tt`, see Text mode), and the Copy menu is last (INT-03).

The Copy menu (`ui/dropdown-menu.tsx`) has three items, each copying through `copyText` from its click:

- "Copy sender address": the part of `MessageMeta.from` between angle brackets, else the whole field, trimmed; "This message has no sender" without one.
- "Copy link (mp://)": the message's selector, what `y` copies (on a draft in Drafts `y` copies its file path).
- "Copy subject": the subject; "This message has no subject" without one.

The notice line says "Copied <address>", "Copied <selector>" or "Copied the subject", or "The clipboard refused ..." when the webview refuses the write.
The palette's READER rows "Copy sender address", "Copy link (mp://)" and "Copy subject" run the same copies on the message the reader shows, and say "Open a message first" without one; they hide with the other selection actions outside Mail and over the outbox view.
The copies have no key of their own: `y` stays the selector's, since a `y` family would turn it into a prefix the TUI does not have.
While the menu is open it owns the keys, as a dialog does.

## Invitations

An email the store marks as an invitation (`MessageMeta.invite`) shows its invitation card under the header block (`InviteCard.tsx`, a region named "Invitation"), the Calendar view's event card with the reply buttons as its children ([shell.md](shell.md), "Calendar").
The card reads `invite_get` once the reader shows the email, and a skeleton stands in until it lands.
An email whose calendar data the store could not read says "This email carries no invitation the store could read".
`state.invites` keeps each card by `readerKey`, and a card goes stale with its account's agenda: a `state.invalidate` or `state.remove` of the account's mailboxes or messages, its `sync.completed`, a bootstrap, and a settled RSVP.
So an update or a cancellation that lands in the inbox, and the fixture's `invite_update` and `invite_cancel`, reach an open card without a reload.

Below the details, the group "Reply to the invitation" holds Accept, Tentative and Decline.
A button sends `calendar_rsvp` with its answer at once, and the three stay disabled while the reply is sending ("Sending Accept…").
The TUI refuses some invitations, and the card disables the three with the TUI's sentence under them, `aria-describedby` from each button, checked in this order (`rsvpRefusal` in `src/app/rsvp.ts`):

1. not a `REQUEST`: "Only received invitations (REQUEST) can be RSVP'd";
2. cancelled: "This event was cancelled by the organizer; nothing to RSVP";
3. superseded: "A newer version of this invitation has arrived; RSVP from that one";
4. an email in the Sent mailbox, the user's own: "You are the organizer of this invite; nothing to RSVP";
5. a Graph account: the daemon's sentence, from `invite_refusal`, read once per account and forgotten when another daemon instance answers.

The TUI's sentences carry an em-dash where the desktop has a semicolon.
With none of these, the line under the buttons says that `t v` opens the reply from the keyboard.

`tv`, from the list or the reader, opens the RSVP choice for the cursor email after the TUI's guards: "Not a calendar invite" for an email that is none, the organizer sentence for one in the Sent mailbox, "This search hit has no local copy to RSVP from" for a server-only hit, then the card's reasons, read with `invite_get` when the card is not loaded.
The choice and what a settled reply says are in [shell.md](shell.md), "Calendar".

## Attachments

The header block lists the message's attachments from `MessageMeta.attachments`, each with its name, its size, and two icon buttons named after the file: "Open <name>" (`to`) and "Save <name>" (`ts`).
Open calls `attachment_open`, which hands the daemon's copy of the part to the system opener, and the status line says "Opened: <name>", the TUI's words ([rust-layer.md](rust-layer.md), "Attachments").
Save opens the Save dialog for that one part.

The keys act on the cursor message from the list or the reader, as the TUI's MESSAGE keys do:

- `to` opens a message's only attachment at once, and over several opens the "Open attachment" dialog, one button per part, the first focused; `j`/`k` and the arrows move between the parts, Enter opens the focused one, `q` closes.
- `ts` opens the Save dialog with every part checked.
- A message with none says "No attachments", the TUI's line.

The Save dialog ("Save attachment", or "Save attachments" over several parts) has a checkbox per part and a Directory field, the TUI's directory prompt, with "Browse…" beside it, the system's folder picker.
The field starts at `~/Downloads`, the TUI's default, and then at the last directory a save went to in this window.
It takes an absolute path or one starting with `~`; a refusal, such as a relative path, shows in the dialog's alert, which keeps it open.
Browse opens the folder picker at the field's directory, `~` expanded (a relative or empty field leaves the system's choice), and a folder picked fills the field, written with `~` when it lies under the home directory, then moves the focus to Save; closing the picker changes nothing.
The picker is `tauri-plugin-dialog`'s `open` (`directory: true`), allowed by `dialog:allow-open` ([rust-layer.md](rust-layer.md), "App CSP").
Where no picker opens, such as a browser under `pnpm dev`, the alert says "The file picker did not open (<why>); type the path instead" and the field stays; the vitest mock answers whatever path a test sets.
A save that went through closes the dialog and says "Saved 2 files to ~/Downloads" in the activity area, with the directory as typed; parts that failed turn it into an alert naming why.

## The browser rendition

`tb`, the toolbar's Open in browser, or the palette's "Open HTML in browser" hands the message to the default browser through `html_open`: the daemon's rendition, with the charset, the CSP tag and the `cid:` images inlined.
The status line says "Opened in browser", or "No HTML version available" for a message whose sender wrote no markup.
On a server-only hit, `tb` writes the hit's own markup into the app cache through `hit_html_open`, and a hit without markup gets the same line; a draft has no rendition.

## Drafts and server-only hits

The reader pane shows no frame for these two.

A selected draft shows `DraftPreview.tsx` (`src/components/compose/`): the `draft_preview` record, which is the headers (From, To, Cc, Bcc), the status pill, the whole body, which `draft_preview` asks with `full: true` where the CLI's dry run cuts it at 500 characters (PERSO-101), and the file path.
Under the headers, `draft_validate`'s report says "Valid" or "Not sendable" with the error, and lists the warnings.
Under the report, the "Attachments" list shows each entry of the draft's `attachments:` as the file spells it, from `draft_attachments`, with "Open <entry>" and "Remove <entry>" buttons.
An entry with no file behind it carries a "missing" badge and its Open is disabled, since the send would fail on it.
Remove calls `draft_attachment_remove`, which takes the entry out of the file and leaves the file it named alone.
The three are read again whenever the listing's row or the list itself changes, which is what a save in the editor or an attach does through `draft.changed`.
Its toolbar, "Draft actions", has Edit in editor (`e`), Edit recipients (`ce`), Attach file (`ta`), and Approve (`cA`) or Back to draft (`cD`).
Approve and Back to draft act on the draft the preview shows and ask nothing, even with drafts marked; a draft being sent is refused with a notice, as by the keys.

Attach file, `ta` in Drafts, opens the "Attach file" dialog, whose File field takes an absolute path or one starting with `~`, stored in the draft as typed.
Its "Browse…" opens the system's file picker the same way as the Save dialog's, and a file picked fills the field, under the home directory as `~/…`, so a draft keeps a path that resolves on another machine's home too; Attach or Enter then attaches it.
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
The notice leaves by itself after twenty seconds (`STICKY_MS`, as the activity area's failures), never while the pointer rests on it, and a newer blocked link starts the time again; the palette's "Show intercepted links" still lists it.
"Open in browser" is the reader's only caller of `open_external`, and only for http, https and mailto; the button is disabled for any other scheme.
The other caller is the device-code dialog's "Open verification page" ([shell.md](shell.md), "The device-code dialog").
The stub's own log line (`source: "open_external_stub"`) raises no notice.
The palette action "Show intercepted links" opens `InterceptedLinksDialog.tsx`, which calls `intercepted_urls()`; that command drains the Rust log, so its answer is merged into `state.intercepted` and the dialog lists the model's copy, newest first.

## Guard verification, 2026-09-30

Run: macOS 26.6.2, WKWebView, `MP_DESKTOP_STUB_OPENER=1 MP_DESKTOP_FIXTURE=1 pnpm tauri dev` in a herdr tab, fixture row 1006 (the hostile message).

Mouse and keyboard input into the app could not be automated.
`cliclick` is not installed, and `AXIsProcessTrusted()` is false for the terminal the agent runs in, so neither `osascript`/System Events nor posted `CGEvent`s reach the window.
Page JavaScript cannot click inside the frame either: the frame is cross-origin to the app, and its only script, the bridge, scrolls and forwards keys and clicks nothing, so a click inside it needs a real input event.
This run predates the bridge (PERSO-81), when the sandbox was `allow-popups` alone; the manual cases below cover the bridge and are to be run again with it.

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

## Known limitations

- Tab inside the frame is the app's pane cycle, so the keyboard does not walk the message's links; a link is refused at either hook anyway (see Refused links).

## Manual verification

Cases (a), (b) and (c) of the table above need a real click, which the agent cannot post (see the run above); the owner ran these steps on 2026-09-30 and all three held, including step 6, whose stub line replaces the browser.
Steps 7 to 10 cover the bridge (PERSO-81) and have not been run yet.
Repeat them after any change to the scheme handler, the bridge, the navigation hooks or the frame's `sandbox`.

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
6. Show the notice again with step 3 and click "Open in browser" once.
   Expected: exactly one `[open] stubbed: https://evil.example/?from=plain-link` line and no browser window; the palette's "Show intercepted links" lists it as "opener stub".
7. Case (f), sender script: reload row 1006 (`k` then `j` from the list) and wait 4 s after the body shows.
   Expected: the message carries `<script>` setting `document.title` and navigating to `?from=script`, and an `onload` handler opening `?from=onload`; no `[nav] refused` line with `from=script` or `from=onload` appears, no notice shows, and the frame still shows the message.
   In the Web Inspector (right-click in the frame, Inspect Element, where the dev build allows it), the console shows CSP refusals for the inline script and the `onload` handler and none for the bridge.
   In its Network tab, the `mpmsg` response's `Content-Security-Policy` header ends in `script-src 'nonce-<n>'`, the same `<n>` as the meta at the top of the document; reloading the row (`k` then `j`) shows another `<n>`.
8. Case (g), the bridge scrolls: Tab to the reader (or `gr`), then press `j` a few times, `Ctrl+d`, `G` and `gg`.
   Expected: the message body itself scrolls by a line, by half a page, to its end and back to its top; `gg` also brings the header block back into view; the list selection does not move.
9. Case (h), keys from inside the frame: click in the message body's text, then press `j`, `Space` and `G`.
   Expected: `j` and `G` scroll the body as in step 8, `Space` opens the which-key popup of the Space family and does not page the frame, `Escape` closes it, and `J` opens the next message; Cmd+C on selected message text still copies it.
10. Case (i), the text rendition: in html mode open row 1002 (no markup; shrink the window until its text overflows), press `j`, `Ctrl+d` and `G`, then click in its text and press `j` and `Escape`.
    Expected: the plain text scrolls in its frame as in steps 8 and 9, and the keys after the click reach the app; in the Web Inspector the `mpmsg` response carries `X-Mp-Rendition: text` and a `script-src 'nonce-<n>'` matching the meta and the one `<script>`.

The verdict of each step goes into the table above, with the date of the run.
