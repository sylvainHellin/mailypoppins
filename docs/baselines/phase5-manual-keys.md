# Phase 5 manual key checklist (oracle d)

The twenty `KeyAction::Manual` rows of `KEYMAP`, pressed by hand against a daemon-backed TUI, one row per key.
It is the companion of [pre-daemon/manual-keys.md](pre-daemon/manual-keys.md), which describes what each key does on the pre-daemon binary; this file records what it did after the cutover.
`tests/phase5_parity_gate.rs::the_phase_five_manual_checklist_is_complete_and_carries_no_failure` parses the table below, so its shape is fixed: one row per `(surface, key)` pair of `mp dump-keys --json`, each present exactly once, with a status of exactly `pass` or `NOT TAKEN (owner-only)`.
A failing key is not a status this document may carry: it is a bug to fix before the gate closes.

## Provenance

- Commit: `36deca3` on branch `daemon`, plus this unit's own commits.
- Walked: 2026-09-21.
- Binary: `target/release/mp` built from that tree with `cargo build --release --offline`, talking to `mp daemon run` from the same binary.
- Fixture: `cargo run --release --example mkfixture -- --out /var/tmp/mp-p5u11-fixture --rows 5000`, the Phase 0 fixture with the Phase 0 flags (two accounts, 5501 messages, one 10 MiB body, every seventeenth message carrying one attachment).
- Terminal: a 120x40 pty, the golden-frame size, driven key by key with the screen read back after each press.
- Environment: `HOME`, `MAILYPOPPINS_DATA_DIR` and `MAILYPOPPINS_CONFIG_DIR` all inside the sandbox, `MAILYPOPPINS_DAEMON_REQUIRE=1` so a call answered in the client's own process would have failed instead of passing, and the daemon started beside the TUI rather than by it (an auto-started daemon inherits `MAILYPOPPINS_DAEMON_REQUIRE` and refuses to start under it).
- Host: Ubuntu 26.04 LTS, Linux 7.0.0-22-generic, AMD Ryzen 7 PRO 8845HS, 28 GiB RAM, headless: no X11 or Wayland display, no browser, no mail server.
- `$EDITOR` and the system opener (`open`) were recording stubs that append their argument to a file and exit 0, because a headless host has neither an interactive editor nor a viewer; every other leg of those keys is the product's own.

Seventeen of the twenty pass.
The three that do not are the three whose effect leaves the process: the clipboard, a server-only search hit, and a browser over a message with an HTML part.
Each says in its own row what a host with a display, a browser and a real account would have to press to close it.
The result list's `cF` and `te` (PERSO-174), the event card's `p`, `Tab` and `m` (PERSO-255) and its `V`, `E`, `D`, `e`, `j/k` and `Esc / q / Enter` (PERSO-270) came after this walk; their rows say so and are not taken until the next walk presses them.

## The table

| surface | key | status | evidence |
|---|---|---|---|
| SERVER SEARCH | `j/k` | pass | Headers pane followed the cursor through the hit list: `Sender 2286` to `Sender 78` to `Sender 2519` on three `j`, back to `Sender 78` on `k`. |
| SERVER SEARCH | `gg / G` | pass | `G` put the last hit (`Sender 1839`) in the headers pane, `gg` put the first (`Sender 2286`) back. |
| SERVER SEARCH | `d/u` | pass | The hit preview scrolled half a page: the body pane went from `estimate steel survey budget permit estimate` to `schedule drawing signature estimate site b…` on `d` and back on `u`. |
| SERVER SEARCH | `Enter` | pass | The overlay closed, the sidebar cursor moved to `Bulk` (the hit's mailbox) and the headers pane showed `Subj: Steel steel 2286`, the hit that had focus. |
| SERVER SEARCH | `e` | pass | The `$EDITOR` stub recorded `/tmp/mailypoppins-4770/render/steel-survey-4420.md`, the read-only Markdown rendition of the hit (RD-06). |
| SERVER SEARCH | `y` | NOT TAKEN (owner-only) | The key ran and the overlay reported `access clipboard: Unknown error while interacting with the clipboard: X11 server connection timed out`. This host is headless, so `arboard` has no clipboard to write the path to. Needs a desktop session. |
| SERVER SEARCH | `f` | NOT TAKEN (owner-only) | The key ran and the overlay reported `Already in the local store`, which is its documented answer for a hit that is not server-only. Every hit over an offline fixture is local, so the fetch leg (`ingest_search_hit`, `fetch_search_hit`, both still store and IMAP residue) needs an account with a server. |
| SERVER SEARCH | `r / R` | pass | `r` wrote `accounts/alpha/drafts/2026-09-21-1411_sender-2286_re-steel-steel-2286.md` and `R` wrote the `-1` sibling beside it, both handed to the `$EDITOR` stub; the sidebar Drafts count rose with them. |
| SERVER SEARCH | `cF` | NOT TAKEN (owner-only) | Added by PERSO-174 after this walk, so never pressed here. `src/tui_tests/actions_store.rs::the_search_overlays_te_exports_a_server_only_hits_eml` presses it against a daemon-backed app and sees `SearchResultForwardAsAttachment` queued; the forward of a server-only hit needs an account with a server to download its `.eml` from. |
| SERVER SEARCH | `te` | NOT TAKEN (owner-only) | Added by PERSO-174 after this walk, so never pressed here. The same test presses it, sees `SearchResultExport` queued and reads the daemon's `.eml` byte for byte; a server-only hit the store has not caught up with needs an account with a server. |
| SERVER SEARCH | `w` | pass | `w` wrote `accounts/alpha/drafts/2026-09-21-1411_sender-2286_fwd-steel-steel-2286.md` and opened it in the `$EDITOR` stub. |
| SERVER SEARCH | `a` | pass | The hit left the result list, the cursor advanced to the next one, and `mp list-messages --mailbox archive` answers `mp://alpha/archive/alpha-bulk-4420@fixture.invalid` where the row had been in `Bulk`; the sidebar counts moved with it (Archive 102 to 103, Bulk 5000 to 4999). |
| SERVER SEARCH | `B` | NOT TAKEN (owner-only) | Added by PERSO-261 after this walk, so never pressed here. `clients/tui/src/block_tests.rs::b_in_the_search_results_blocks_a_hit_in_the_store` presses it on a hit in the local store, sees the chooser open on its sender and return to the results on Esc and on Enter, `sender.block` called with the picked entry and the results' status line saying what the block came to, and a server-only hit refused there; the moves it drives need an account with a server and a Junk folder. |
| SERVER SEARCH | `b` | NOT TAKEN (owner-only) | The key ran, reached the daemon and was refused: `message.materialise_html failed: this message carries no HTML to render (-32602)`. No message the fixture generates carries an HTML part, and the host has no browser to hand one to. Needs a real account and a desktop session. |
| SERVER SEARCH | `o` | pass | The daemon materialised the part into a handle of its own and the opener stub was handed `runtime/handles/9d350fbfe9f1bee0abe3c50e588a4470/attachment-153.txt`, which is where P5-U6 moved the per-row materialisation directory. |
| SERVER SEARCH | `O` | pass | The dir picker opened on `/var/tmp/mp-p5u11-walk/Downloads` and `Enter` on `[ Save here ]` wrote `attachment-153.txt`, 23 bytes, the attachment's own payload. |
| SERVER SEARCH | `S` | NOT TAKEN (owner-only) | Added by PERSO-273 after this walk, so never pressed here. `src/tui_tests/actions.rs::s_in_the_results_cycles_the_order_and_runs_the_search_again_in_it` presses it three times against a daemon-backed app and sees both passes ask for `oldest`, `relevance` and `newest` in turn, the local hits come back in that order and the status line name the order the daemon echoed; the server leg's `hit_order` needs an account with a server. |
| SERVER SEARCH | `Tab` | pass | Focus left the result list for the Advanced field: the footer changed from `Enter: open / e: read / y: path / …` to `Tab/Shift+Tab: fields / Space: toggle / Enter: search / Esc: close` and the cursor block landed on the Advanced line. |
| SERVER SEARCH | `Esc` | pass | The overlay closed from the result list and the mail view was painted underneath it, cursor and counts intact. |
| EVENT CARD | `V` | NOT TAKEN (owner-only) | Catalogued under the card by PERSO-270 after this walk, so never pressed here. `src/tui_tests/calendar.rs::v_on_the_card_opens_the_respond_overlay_for_its_event` opens the card with `Enter` and presses it, and sees the respond overlay on the card's event, or on a cancelled meeting the card closed and the refusal on the status line; sending the answer needs an account with a server. |
| EVENT CARD | `E` | NOT TAKEN (owner-only) | Catalogued under the card by PERSO-270 after this walk, so never pressed here. `src/tui_tests/calendar.rs::shift_e_on_the_card_opens_the_event_form_with_its_values` presses it and sees the edit form holding the event's title, times and place, or on an invitation mail's event and on a calendar mp does not write to, the refusal on the status line; saving the change needs an account with a server. |
| EVENT CARD | `D` | NOT TAKEN (owner-only) | Catalogued under the card by PERSO-270 after this walk, so never pressed here. `src/tui_tests/calendar.rs::d_on_the_card_opens_the_cancel_confirmation_or_refuses_a_foreign_meeting` presses it and sees the cancel confirmation of the user's own meeting with its two attendees, or on someone else's meeting the refusal pointing at `V`; the cancel itself needs an account with a server. |
| EVENT CARD | `p` | NOT TAKEN (owner-only) | Added by PERSO-255 after this walk, so never pressed here. `src/tui_tests/calendar.rs::p_on_the_card_opens_the_edit_form_at_the_proposed_time` presses it on the organizer's card and sees the edit form at the marked proposal, an occurrence's in the occurrence scope; taking one against a server needs an account whose attendee proposed a time. |
| EVENT CARD | `Tab` | NOT TAKEN (owner-only) | Added by PERSO-255 after this walk, so never pressed here. The same test presses it twice on a card with two proposals and sees the mark come back to the first. |
| EVENT CARD | `m` | NOT TAKEN (owner-only) | Added to the catalogue by PERSO-255 after this walk, so never pressed here, and no test presses it on the card; it opens the invitation mail an `imip:` event came from, which the walk's fixture has none of. |
| EVENT CARD | `e` | NOT TAKEN (owner-only) | Catalogued under the card by PERSO-270 after this walk, so never pressed here. `src/tui_tests/calendar.rs::e_on_the_card_opens_the_invitations_ics_only_for_a_mail` presses it on an invitation mail's event and sees `OpenEventSource` queued for its row, the CALENDAR `e`'s action, and on a server event sees the card close with nothing queued; the `.ics` reaching a real `$EDITOR` needs a desktop session. |
| EVENT CARD | `j/k` | NOT TAKEN (owner-only) | Catalogued under the card by PERSO-270 after this walk, so never pressed here. `src/tui_tests/calendar.rs::j_and_k_scroll_the_card` presses `j`, `Down`, `k` and `Up` on a card taller than a twenty-row frame and sees the offset move by a line each, the card's last line come into view and leave again, and `k` stop at the top. |
| EVENT CARD | `Esc / q / Enter` | NOT TAKEN (owner-only) | Catalogued under the card by PERSO-270 after this walk, so never pressed here. `clients/tui/src/app/keys.rs::calendar_enter_opens_the_event_card` presses `Esc` on the card and `src/tui_tests/calendar.rs::q_and_enter_close_the_card` presses `q` and `Enter`, and each sees the card close over the Calendar view, `q` without quitting. |
| ACTIVITY LOG | `j/k` | pass | With 100 entries in the log, `j` moved the top visible line on by one and `k` moved it back. |
| ACTIVITY LOG | `d/u` | pass | `d` moved the top visible line on by half a window (from the first entry to the middle of the second), `u` moved it back to where `gg` had left it. |
| ACTIVITY LOG | `gg / G` | pass | `G` put the tail of the log on screen (entry `n46` of the ladder at the top of the window), `gg` put the oldest entry back. |
| ACTIVITY LOG | `/` | pass | The footer changed to `Type to filter / Esc: clear`, the prompt took `n37`, and the title went from `Activity Log (100)` to `Activity Log (25)`. |
| ACTIVITY LOG | `Esc` | pass | The first `Esc` cleared the filter and the title went back to `Activity Log (100)`; the second closed the overlay. |

## How the log was made scrollable

The activity overlay holds the status lines a session produced, and a fresh session has three.
Scrolling a three-entry log proves nothing, so the log was filled to its 100-entry cap with a ladder of distinguishable entries: `gt` (jump to date) with a token of its own each time (`n01` to `n60`), each of which leaves `Cannot read 'nNN' as a date` on the activity line.
A key's effect is then readable off the screen rather than inferred, which is the point of a manual checklist.

## What this walk also showed

The mutations went through the daemon and nothing else: `a` moved a row between mailboxes and both sidebar counts followed, `r`, `R` and `w` wrote their drafts through `draft.reply` and `draft.forward` and the Drafts count followed, and `o` was answered out of `runtime/handles/` rather than out of a per-row materialisation directory in the client's temp tree.
Two keys reached a refusal the daemon worded (`b` and `f`), and both refusals are the ones the parity matrix predicts for a fixture with no HTML part and no server.

One thing worth a follow-up, recorded rather than fixed here: `b` on a hit whose message carries no HTML logs the refusal and puts nothing on the overlay's status line, where the same key outside the overlay says so on screen (`a_message_without_html_says_so_instead_of_opening_an_empty_page`).
On a headless host the difference is invisible; on a real one it is a key that looks like it did nothing.
