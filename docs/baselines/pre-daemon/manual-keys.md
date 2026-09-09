# Overlay-internal keys (ANO-2)

Every key the pre-daemon TUI dispatches by hand, outside the `KEYMAP` resolver.
`mp dump-keys` sees only `KEYMAP` rows, so `tui-keys.json` in this directory covers the Normal-mode
surface and nothing else; the keys below are the part of the input surface a GUI port would silently
drop.

Scope and method: every `match key.code` arm in the non-test half of `src/tui/app/keys.rs`
(lines 1-3056) that is not reached through `keymap::resolve`.
Line numbers are the arm's first line at commit `f8af44b` (the `pre-daemon` freeze).
`src/tui/event.rs` hands every `KeyEventKind::Press` straight to `App::handle_key`, and no other
file in `src/` mentions `KeyCode`, so this file plus `KEYMAP` is the whole keyboard surface.

Counts: 24 surfaces, 152 hand-dispatched match arms, 192 key spellings over 41 distinct spellings
(an arm such as `j | Down` is one arm and two spellings, and a spelling live in two surfaces counts
in each).

`KEYMAP` carries 20 documented-only `KeyAction::Manual` rows, all of them for two of these surfaces:
15 for the server search result list (`src/tui/app/keymap.rs:675-689`) and 5 for the activity overlay
(734-738).
The other 22 surfaces have no catalogue row at all, so they reach neither the help overlay nor the
website table; several advertise themselves in an on-screen footer instead, and the ones that
advertise nothing are listed at the end.

## Undo-send hold prompt (no overlay)

Live only while `held_send` is set, ahead of the table dispatch in `dispatch_normal_mode`.

- `u` (83) cancels the parked send before it reaches SMTP; the draft is untouched.
  Shadows the `KEYMAP` `u` (toggle read) for the length of the hold window.

## Confirm dialog

`handle_confirm_key` (883). The footer renders `[y]es [n]o` (`src/tui/ui/overlays.rs:101`).

- `y` / `Enter` (885) runs the confirmed action: batch approve, mark-draft, archive or delete over
  the selection, the single-message variant otherwise, or the signature delete, then closes and
  promotes any error queued behind the dialog.
- `n` / `Esc` (981) cancels; a cancelled signature delete returns to the signatures overlay rather
  than to the mail view.

## Server search overlay, form focus

`handle_search_form_key` (1012). Catalogued as `SERVER SEARCH` in part; the form's own keys are not.

- `Esc` (1022) closes the overlay.
- `Tab` (1025) moves to the next enabled field, skipping the fields a non-blank Advanced line greys
  out.
- `Shift+Tab` (1032) moves to the previous enabled field, same skip.
- `Down` (1039) drops from the Advanced line into the result list when there are hits.
- `Space` (1046) toggles the scope between current mailbox and current account, on the Scope field.
- `Space` (1049) toggles the has-attachment filter, on the Attachment field, unless the Advanced
  line is active.
- `Enter` (1052) builds the query AST and dispatches the search; a blank form is a no-op and a parse
  error is shown verbatim.
- `Backspace` (1055) deletes the last character of the focused text field and clears the status line.
- printable char (1063) appends to the focused text field; a Ctrl-prefixed char returns without
  editing (1065).

## Server search overlay, result list

`handle_search_overlay_list_key` (1139). With no results only three arms are live.

- `Tab` / `Shift+Tab` (1143) returns focus to the Advanced field, no results case.
- `Esc` (1146) closes the overlay, no results case.
- `j` / `Down` (1155) moves to the next hit and clears the result-count message.
- `k` / `Up` (1165) moves to the previous hit, same clear.
- `g` (1173) arms the `g` prefix; `gg` jumps to the first hit.
- `G` (1184) jumps to the last hit.
- `d` (1190) scrolls the hit preview a half page down.
- `u` (1193) scrolls it a half page up.
- `Enter` (1196) opens the hit in the mail list (`Action::SearchResultJump`).
- `e` (1199) opens the hit read-only in `$EDITOR`.
- `y` (1202) copies the Markdown rendition path.
- `f` (1205) fetches a server-only hit into the store.
- `r` (1208) replies to the hit.
- `R` (1211) replies to all.
- `w` (1214) forwards it.
- `a` (1217) archives it.
- `b` (1220) opens its HTML part in the browser.
- `o` (1223) opens the attachment picker in Open mode over the hit.
- `O` (1226) opens it in Save mode.
- `Tab` / `Shift+Tab` (1229) returns focus to the Advanced field.
- `Esc` (1232) closes the overlay.

## Activity overlay, filter input

`handle_activity_overlay_key`, `activity_filter_active` branch (1245).

- printable char (1248) appends to the filter and resets the scroll.
- `Backspace` (1252) deletes the last filter character, same reset.
- `Enter` (1256) leaves the input and keeps the filter applied.
- `Esc` (1259) clears a non-empty filter and leaves the input; on an empty filter it closes the
  overlay.

## Activity overlay, browse

Same function, the `else` branch.

- `j` / `Down` (1272) scrolls one line down.
- `k` / `Up` (1276) scrolls one line up.
- `g` (1280) arms the `g` prefix; `gg` jumps to the top.
- `G` (1289) jumps to the bottom (`u16::MAX`).
- `d` (1293) scrolls a half page down.
- `u` (1297) scrolls a half page up.
- `/` (1301) arms the filter input and clears the previous filter.
- `Esc` / `L` / `q` (1307) closes the overlay and resets scroll and filter.
  Only `Esc` is catalogued or advertised; `L` is the pre-#0092 opener kept as a closer.

## Compose wizard

`handle_compose_wizard_key` (1326). The field footers in `src/tui/ui/compose.rs:55-67` advertise most
of these; the wizard has no `KEYMAP` context.

- `Esc` (1333) cancels the wizard (`Action::ComposeWizardCancel`).
- `Tab` (1337) moves to the next field and recomputes the contact suggestions.
- `Shift+Tab` (1343) moves to the previous field, same recompute.
- `Up` (1349) cycles the signature backwards on the Signature field, otherwise moves the suggestion
  highlight up on an address field.
- `Down` (1360) cycles the signature forwards, otherwise moves the suggestion highlight down.
- `Ctrl+g` (1371) force-submits from any field.
- `Ctrl+e` (1376) opens the selected signature in `$EDITOR`, on the Signature field only.
- `Ctrl+n` (1382) same as `Down`.
- `Ctrl+p` (1393) same as `Up`.
- `Ctrl+u` (1404) clears the current field; a no-op on the Signature selector.
- `Enter` (1414) is field-dependent: advance from Signature, accept the highlighted suggestion on an
  address field, insert a newline in the Body, submit from Subject when the mode has no body field,
  advance otherwise.
- `Backspace` (1452) deletes the last character; a no-op on the Signature selector.
- printable char (1463) appends to the field and recomputes suggestions on an address field; on the
  Signature field `e` opens `$EDITOR` and every other char is a no-op; Ctrl-prefixed chars are
  ignored.

## Thread overlay

`handle_thread_overlay_key` (1721). Footer: `j/k nav  Enter open  Esc close`.

- `j` / `Down` (1727) moves down one thread message.
- `k` / `Up` (1732) moves up one.
- `g` (1735) jumps to the first message, on a bare `g` with no `gg` pair.
- `G` (1738) jumps to the last.
- `Enter` / `e` (1741) closes the overlay and opens the selected message.
- `Esc` / `q` / `T` (1748) closes the overlay.

## RSVP overlay

`handle_rsvp_overlay_key` (1756). Footer: `Enter confirm  ·  Esc cancel`.

- `j` / `Down` / `Tab` (1759) moves to the next choice.
- `k` / `Up` / `Shift+Tab` (1764) moves to the previous.
- `a` (1767) selects Accept.
- `t` (1768) selects Tentative.
- `d` (1769) selects Decline.
- `Enter` (1770) queues `Action::Rsvp` with the selected choice, closes, and promotes a queued error.
- `Esc` / `q` (1788) closes without replying.

## Attachment picker, Open mode

`handle_attachment_picker_key` (1796). Footer: `j/k nav  Enter open  Esc cancel`.

- `j` / `Down` (1800) moves down one file.
- `k` / `Up` (1805) moves up one.
- `Enter` (1808) opens the highlighted attachment (`Action::OpenAttachment`) and closes.
- `Esc` / `q` (1819) closes.

## Attachment picker, Save mode

Same function, the `AttachmentPickerMode::Save` arm.
Footer: `j/k nav  Space sel  ^a all  Enter confirm  Esc cancel`.

- `j` / `Down` (1825) moves down one file.
- `k` / `Up` (1830) moves up one.
- `Space` (1833) toggles the file in the multi-select set and advances the cursor.
- `Ctrl+a` (1845) selects every file, or clears the set when it is already full.
- `Enter` (1852) hands the selected files (or the cursor file when nothing is ticked) to the dir
  picker, an overlay-to-overlay handoff that deliberately does not promote a queued error.
- `Esc` / `q` (1877) closes.

## Dir picker, zoxide mode

`handle_dir_picker_key` (1886). Footer: `Tab browse  Enter save  Esc cancel`.

- `Down` (1894) moves down one zoxide result.
- `Up` (1901) moves up one.
- `Backspace` (1904) deletes the last query character and re-runs the zoxide query.
- `Tab` (1909) switches to browser mode, starting at the highlighted result.
- `Enter` (1921) saves the attachments into the highlighted directory and closes.
- `Esc` (1931) closes.
- printable char (1934) appends to the query and re-runs it.

## Dir picker, browser mode

Same function, the `DirPickerMode::Browser` arm.
Footer: `Tab zoxide  Enter open/save  h up  ~ home  Esc cancel`.

- `j` / `Down` (1942) moves down one entry.
- `k` / `Up` (1948) moves up one.
- `g` (1951) jumps to the `[ Save here ]` row, on a bare `g` with no `gg` pair.
- `G` (1954) jumps to the last directory.
- `~` (1957) jumps to `$HOME`.
- `h` / `Backspace` (1964) goes to the parent directory.
- `l` (1971) descends into the highlighted directory.
- `Enter` (1982) saves into the current directory on row 0, otherwise descends.
- `Tab` (2002) switches back to zoxide mode.
- `Esc` (2007) closes.

## Quick-move mailbox picker

`handle_mailbox_picker_key` (2066). The footer names the filter, the arrow keys, `Enter move` and
`Esc cancel`.

- `Down` / `Tab` (2070) moves down one candidate.
- `Up` / `Shift+Tab` (2077) moves up one.
- `Enter` (2080) queues `Action::MoveToMailbox` for the selection, clears it, closes, and promotes a
  queued error; a no-op when the filter matches nothing.
- `Esc` (2097) closes without moving.
- `Backspace` (2100) deletes the last filter character and refilters.
- printable char (2104) appends to the filter and refilters.

## Signatures overlay, browse

`handle_signatures_browse_key` (2145).
Footer: `j/k nav  Enter default  e edit  n new  r rename  d delete  Esc close`.

- `j` / `Down` (2147) moves down one signature.
- `k` / `Up` (2154) moves up one.
- `Enter` (2159) sets or clears the account default.
- `e` (2160) queues `Action::EditSignatureFile`, or says there is nothing to edit.
- `n` (2168) opens the create-name prompt.
- `r` (2174) opens the rename prompt seeded with the current name.
- `d` (2188) opens the delete confirm dialog.
- `Esc` / `q` (2189) closes the overlay.

## Signatures overlay, name prompt (New and Rename)

`handle_signatures_prompt_key` (2197). Footer: `Enter confirm  Esc cancel`.

- `Esc` (2203) returns to the browse list and clears the input.
- `Backspace` (2209) deletes the last input character.
- `Enter` (2214) commits the create or the rename, reporting a validation error on the status line
  and keeping the prompt open.
- printable char (2219) appends to the name.

## Command palette

`handle_command_palette_key` (2362). The footer names the filter, the arrow keys, `Enter run` and
`Esc cancel`.
The opener (`:` / `Ctrl+p`) is a `KEYMAP` row; everything inside is not.

- `Down` / `Tab` (2368) moves down one action.
- `Up` / `Shift+Tab` (2375) moves up one.
- `Enter` (2378) closes and runs the selected `KeyAction` through `execute` in the context the
  palette floated over.
- `Esc` (2388) closes.
- `Backspace` (2391) deletes the last query character and refilters.
- printable char (2395) appends to the query and refilters, family leaders included (they are
  literal text here).

## Help overlay, filter input

`handle_help_key`, `help_filter_active` branch (2505).

- printable char (2508) appends to the filter and resets the scroll.
- `Backspace` (2512) deletes the last filter character, same reset.
- `Enter` (2516) leaves the input and keeps the filter.
- `Esc` (2519) clears a non-empty filter and leaves the input; on an empty filter it closes the
  overlay.

## Help overlay, browse

Same function, the `else` branch. Footer: `/filter  j/k scroll`.

- `j` / `Down` (2532) scrolls one line down.
- `k` / `Up` (2536) scrolls one line up.
- `g` (2540) arms the `g` prefix; `gg` jumps to the top.
- `G` (2548) jumps to the bottom.
- `d` (2552) scrolls a half page down.
- `u` (2556) scrolls a half page up.
- `/` (2560) arms the filter input.
- `?` / `Esc` (2566) closes the overlay and resets scroll and filter.

## Persistent error overlay

`handle_persistent_error_key` (2581). Footer: `[s]ync now  [d]ismiss`.

- `s` (2583) closes the overlay and queues a sync.
- `d` / `Esc` (2587) dismisses it.

## Metadata search prompt (`Focus::Search`)

`handle_search_key` (2595), armed by the `KEYMAP` `fm` binding. The prompt borrows the list's
one-line slot with a `/` prefix (`src/tui/ui/list.rs:200-225`).

- `Enter` (2597) leaves the prompt for the list and keeps the filter.
- `Esc` (2600) clears the query, restores the full list, and returns focus to the list.
- printable char (2605) appends to the query and refilters, narrowing the visible set in place when
  the lowercased query only grew (the Greek final-sigma case falls back to a full recompute).
- `Backspace` (2621) deletes the last character and recomputes from the full list.

## Jump-to-date prompt

`handle_jump_date_key` (2639), armed by the `KEYMAP` `gt` binding. Prompt prefix `date:`.

- `Enter` (2641) parses the typed date against today and moves the cursor to the newest row on or
  before it; an unreadable date leaves the prompt armed with the reason on the status line.
- `Esc` (2654) abandons the prompt.
- printable char (2657) appends to the buffer.
- `Backspace` (2662) deletes the last character.

## Attach-file prompt

`handle_attach_file_key` (2685), armed by the `KEYMAP` `ta` binding. Prompt prefix `attach:`.

- `Enter` (2687) queues `Action::AttachFileToDraft` with the raw text after a `~`-expanded existence
  check; an empty commit cancels, and a path that does not resolve keeps the prompt armed.
- `Esc` (2708) abandons the prompt.
- printable char (2711) appends to the buffer.
- `Backspace` (2716) deletes the last character.

## Contacts view search prompt

`handle_contacts_search_key` (2729), armed by the `KEYMAP` Contacts `/` binding.

- `Enter` (2731) leaves the input and keeps the filter.
- `Esc` (2734) leaves the input, clears the query, and recomputes the full match list.
- printable char (2739) appends to the query and recomputes the matches.
- `Backspace` (2743) deletes the last character and recomputes.

## Keys with no catalogue row and no on-screen hint

Discoverable only by reading `keys.rs`, so a port has to be told they exist.

- `u`, the undo-send cancel (83). Advertised once, in the quit refusal at `src/tui/app/mod.rs:1001`.
- `L` and `q`, the activity overlay's alternative closers (1307).
- `g`, `G`, `q` and `T` in the thread overlay (1735, 1738, 1748).
- `j`, `k`, `a`, `t`, `d`, `Tab`, `Shift+Tab` and `q` in the RSVP overlay (1759-1788); its footer
  names only `Enter` and `Esc`, so the three mnemonic choice keys are invisible.
- `q` in both attachment picker modes (1819, 1877).
- `Down` from the Advanced field into the result list in the search form (1039).
- `g` and `G` in the dir picker's browser mode (1951, 1954).
- `g`, `G`, `d`, `u` and the `?` closer in the help overlay (2540-2566); its footer names only `/`
  and `j/k`.
- `Tab` / `Shift+Tab` as navigation in the mailbox picker (2070, 2077) and the command palette
  (2368, 2375); both footers name only the arrow keys.

## Keys reachable in neither `KEYMAP` nor this file

None. `src/tui/event.rs:36-49` forwards every key press to `App::handle_key` without inspecting it,
`handle_key` routes to exactly one of the surfaces above or to `dispatch_normal_mode`, and
`keys.rs` is the only file under `src/` that mentions `KeyCode`.

Two adjacent facts, since a port will meet them: nothing intercepts `Ctrl+c` (in raw mode it arrives
as an ordinary key event, matches no arm, and is dropped, so quitting is `q` only), and
`KeyEventKind::Repeat` and `Release` are dropped at the event layer, so a held key produces one
action per press.
