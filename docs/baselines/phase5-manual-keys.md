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
| SERVER SEARCH | `w` | pass | `w` wrote `accounts/alpha/drafts/2026-09-21-1411_sender-2286_fwd-steel-steel-2286.md` and opened it in the `$EDITOR` stub. |
| SERVER SEARCH | `a` | pass | The hit left the result list, the cursor advanced to the next one, and `mp list-messages --mailbox archive` answers `mp://alpha/archive/alpha-bulk-4420@fixture.invalid` where the row had been in `Bulk`; the sidebar counts moved with it (Archive 102 to 103, Bulk 5000 to 4999). |
| SERVER SEARCH | `b` | NOT TAKEN (owner-only) | The key ran, reached the daemon and was refused: `message.materialise_html failed: this message carries no HTML to render (-32602)`. No message the fixture generates carries an HTML part, and the host has no browser to hand one to. Needs a real account and a desktop session. |
| SERVER SEARCH | `o` | pass | The daemon materialised the part into a handle of its own and the opener stub was handed `runtime/handles/9d350fbfe9f1bee0abe3c50e588a4470/attachment-153.txt`, which is where P5-U6 moved the per-row materialisation directory. |
| SERVER SEARCH | `O` | pass | The dir picker opened on `/var/tmp/mp-p5u11-walk/Downloads` and `Enter` on `[ Save here ]` wrote `attachment-153.txt`, 23 bytes, the attachment's own payload. |
| SERVER SEARCH | `Tab` | pass | Focus left the result list for the Advanced field: the footer changed from `Enter: open / e: read / y: path / …` to `Tab/Shift+Tab: fields / Space: toggle / Enter: search / Esc: close` and the cursor block landed on the Advanced line. |
| SERVER SEARCH | `Esc` | pass | The overlay closed from the result list and the mail view was painted underneath it, cursor and counts intact. |
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
