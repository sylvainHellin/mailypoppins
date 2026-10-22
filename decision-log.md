# Decision log

Durable decisions settled with Sylvain, newest at the bottom.
Architecture records with their own evidence live under `docs/baselines/decisions/`.

## 2026-10-19 - The conversation list filter matches a conversation by its participants and newest message only

Why: members carry no subject of their own and the five listed senders cover most cases; the gap is a conversation whose only match is an older member from a sixth sender.
Rejected: matching expanded members as well (results would depend on what is expanded); having the daemon return every member's sender and subject for filtering (complete, but a protocol change for a rare case).
Source: chat, PERSO-146 follow-ups.
Status: accepted

## 2026-10-19 - The Graph sync backend stays parked; Graph threading and Graph reply headers wait for it

Why: IMAP covers both accounts in daily use, so PERSO-144 (Graph sends carry no In-Reply-To or References) and PERSO-147 (thread Graph and Gmail rows by server conversation id) are postponed rather than unparking #0042 and #0055 for them.
Rejected: unparking the Graph backend now.
Source: chat.
Status: superseded by 2026-10-21

## 2026-10-21 - The Graph backend is unparked with the EVOQS Exchange account as its first live account

Why: EVOQS mail stays on Microsoft 365 (the Proton move, PERSO-73, is cancelled), Sylvain administers that tenant and can consent to Graph himself, and the Graph paths have never met a live server, which only a real account fixes; the gaps it shows (no scheduled tick, no reply headers on Graph sends, no Graph threading, no invitation answers) are unbuilt, not inherent to Graph, and PERSO-144 comes early.
Rejected: OAuth2 over IMAP and SMTP for EVOQS (works with the tested path today but leaves Graph untested); Exchange Web Services (being retired in Exchange Online).
Source: chat.
Status: accepted

## 2026-10-21 - mailypoppins gets its own Microsoft Entra tenant and one multi-tenant app registration, published by EVOQS GmbH

Why: an app registration cannot move between tenants, so a dedicated tenant keeps the client ID with mailypoppins whatever happens to EVOQS; one registration for any Entra tenant plus personal Microsoft accounts lets users sign in without registering their own app, and EVOQS GmbH is the legal entity for publisher verification.
Rejected: registering the app inside the EVOQS tenant (fastest, but tied to EVOQS for good); every user registering their own app (no verification work, but the whole portal procedure for each user).
Source: chat.
Status: accepted

## 2026-10-21 - Graph sends go as MIME through sendMail so replies carry In-Reply-To, References and our Message-ID

Why: one request with the bytes the SMTP path already builds threads a reply for every recipient and keeps the Message-ID the outbox and the local sent copy are keyed on; a message whose base64 MIME exceeds 3.5 MB (Graph caps the request near 4 MB) falls back to the JSON payload without threading, logged as a warning.
Rejected: createReply, PATCH and send (no control over the Message-ID, four round trips, a draft left in Drafts on failure, and it may mark the parent read); internetMessageHeaders on the JSON payload (Graph accepts only x- headers there).
Source: .agents/research/2026-10-21-graph-reply-threading.md, chat of 2026-10-21 (PERSO-144).
Status: accepted

## 2026-10-22 - Graph rows thread by In-Reply-To and References first and by conversationId second

Why: the headers are what IMAP rows and the outbox's own Sent rows thread by, so a conversation threads the same whichever backend brought each message; `conversationId` exists only on Graph rows and threads what the headers cannot, a parent stored after its reply or a reply whose parent the store never held. It is kept in a new nullable `messages.conversation_id` column added on open, `NULL` for never asked and empty for asked and none, so the one-shot re-thread of stored rows needs no schema bump and no re-download. The headers come from the MAPI properties `String 0x1042` and `String 0x1039` through `$expand`, about 0.4 KB a message, not from `internetMessageHeaders`, about 9 KB.
Rejected: `conversationId` as the only signal (Graph and IMAP copies of one conversation would split); `conversationId` first (Exchange groups by its own rules, and a header match is exact); `internetMessageHeaders` (twenty times the payload for the same two values); a schema version bump (drops every store and re-downloads every body).
Source: PERSO-147 brief from Vera's session, live probe of the EVOQS mailbox on 2026-10-22 (`examples/graph_thread_probe.rs`).
Status: accepted

## 2026-10-22 - The desktop prefix chord waits 5 s by default for its second key, configurable in Settings

Why: the 1.2 s wait was too short, so a late second key fired its flat action (`a` after `c` archived instead of replying to all); 5 s leaves room for a slow second key while an accidental prefix still lapses on its own. Ticket [#0148](docs/tickets/0148-chord-timeout.md), plan `docs/plans/chord-timeout.md`.
Rejected: no timeout by default (an accidental prefix keeps the which-key overlay up until Escape; it stays a choice in Settings); 3 s (less room for the second key).
Source: chat with Sylvain, 2026-10-22.
Status: superseded by the 2026-10-22 entry "The desktop prefix chord never times out"

## 2026-10-22 - The TUI sidebar keeps showing only the active account; collapsing the other accounts is desktop only

Why: the TUI sidebar never lists the other accounts, so the request is already met there, and the `ga` picker covers switching. Ticket [#0147](docs/tickets/0147-account-picker.md), plan `docs/plans/account-picker.md` (Decision 6).
Rejected: collapsed header rows for the other accounts in the TUI sidebar (extra work for the same result).
Source: chat with Sylvain, 2026-10-22.
Status: accepted

## 2026-10-22 - mp gets named profiles; the demo mailbox and its fake data live outside the repo

Why: a profile (its own config, data, store and daemon) is useful beyond demos, for testing or a separate work setup, and is the only thing a demo needs from mp; the made-up corpus and its seed script talk to mp only through IMAP and `.eml`, so they cannot drift with mp's internals and stay off the public repo. The demo account is a real Zimbra mailbox, `demo@mailypoppins.com`, seeded over IMAP APPEND from a local repo on the home server (`~/code/side-projects/mailypoppins-demo`); live replies on stage go to one of Sylvain's real addresses.
Rejected: a local fake IMAP/SMTP server (mp needs CONDSTORE, QRESYNC, IDLE, MOVE and UIDPLUS, so it means installing Stalwart or Dovecot, against a real mailbox that needs nothing); a store seeded without a server (archive, send and sync would fail on camera); a `demo/` folder in this repo (fake data in the public repo for no gain).
Source: chat with Sylvain, 2026-10-22.
Status: accepted

## 2026-10-22 - The TUI's default theme is `mailypoppins`, the Mac app's dark palette

Why: the TUI, the Mac app and the website should read as one product, and there are hardly any outside users whose colours an update would change; `theme = "catppuccin-mocha"` brings the old look back.
Rejected: keeping `catppuccin-mocha` as the default with `mailypoppins` as an opt-in (every new user would start on a look the rest of the product dropped).
Source: chat with Sylvain, 2026-10-22.
Status: accepted

## 2026-10-22 - Attachments carry their Content-ID and an inline flag as two additive columns on `message_blobs`

Why: the attachment listings, the paperclip and the forward need a per-reference yes or no they can read in SQL, and the renderer needs the id to turn a `cid:` URL into a stored blob on a Graph row, which has no raw message; two nullable columns added on open keep every stored body, with `is_inline` `NULL` as the "never classified" marker the readers fall back on and the backfill picks up. Inline means an image whose Content-ID the HTML body references, since Exchange gives every attachment an id.
Rejected: one `content_id` column with the inline verdict derived from the HTML at read time (every listing would read every HTML body); not storing inline parts as attachments at all (the renderer needs their bytes, and a misjudged part would be lost rather than hidden); a schema version bump (drops every store and re-downloads every body).
Source: PERSO-165 and #0145 brief from Vera's session; live backfill on a copy of the EVOQS store, 2026-10-22 (`examples/graph_inline_probe.rs`).
Status: accepted, amended by the 2026-10-22 entry "Inline images stay reachable"

## 2026-10-22 - Inline images stay reachable: listed in their own group, saveable, left out only of the forward and the paperclip (amends the PERSO-165 entry)

Why: an iPhone photo, an Outlook pasted screenshot and every other `cid:`-referenced image are sent the same way as a signature logo, so dropping inline references from `read::attachments_for` made them unreachable for `mp save`, `mp open`, the TUI and desktop lists, `message.materialise_attachment` and hooks, and a Graph image above the 8 MB inline cap was then neither rendered nor saved. `attachments_for` lists every reference with an `inline` flag, the `part` index is the position in that full stored-order list (stable before and after the backfill classifies a row), the readers show inline images after the real attachments, and only the forward and the paperclip skip them. On Graph the paperclip also keeps the server's own `hasAttachments`, which is false for inline-only mail, so an attached Outlook item or cloud link beside a logo keeps it.
Rejected: excluding inline references from every listing (the first cut, unreachable photos); a `part` index over the real attachments only (it shifts when the backfill classifies a row a client holds); a new `mp save` flag for inline images (`mp save` already saves every part).
Source: orchestrator review of PERSO-165, 2026-10-22.
Status: accepted

## 2026-10-22 - The account picker follows its plan's five recommendations

Why: a plain list with the digits 1 to 9 and no filter field keeps `j` and `k` as navigation; each row shows new mail as the inbox unread count in the desktop and as a dot in the TUI, which keeps no per-account unread count; a desktop account expanded or collapsed by hand stays so until the app quits, and every start shows only the selected account expanded; in the desktop sidebar `j` and `k` skip collapsed accounts and `ga` is the keyboard path to another account, and picking the current account there expands it again after a hand collapse, since its header takes no key; a row of an account that is not serving says so in both pickers (the runtime state in the desktop, `not ready` in the TUI, which has one flag for opening and blocked); the unreachable `Ctrl+1..9` account jump leaves the TUI code. Ticket [#0147](docs/tickets/0147-account-picker.md), plan `docs/plans/account-picker.md`.
Rejected: a type-to-filter field (it would swallow `j` and `k`); a TUI unread count per account (new state for a cosmetic gain); remembering the expand state across restarts; a cursor stop on every account header with Enter to toggle it (a second keyboard model for the picker's job).
Source: decisions page `.sync/2026-10-22-mailypoppins-open-decisions.html`, section 1, answered 2026-10-22.
Status: accepted

## 2026-10-22 - The desktop prefix chord never times out, as in the TUI

Why: a pending prefix waits for its second key with no timer and no setting, so `c`, a pause of any length, then `a` replies to all in both clients; Escape cancels the prefix and does nothing else, a click drops it so an abandoned chord never resolves with a much later key, and the which-key popup appears at once. Ticket [#0148](docs/tickets/0148-chord-timeout.md), plan `docs/plans/chord-timeout.md`.
Rejected: a 5 s default with four presets in Settings (the earlier entry of the same day; two behaviours between the clients and a setting changed once); a show delay before the popup.
Source: decisions page `.sync/2026-10-22-mailypoppins-open-decisions.html`, section 1, answered 2026-10-22.
Status: accepted

## 2026-10-22 - Server drafts link by Message-ID, then by a single matching draft, and the mirror is on for Proton too

Why: the probe of 2026-10-20 showed that no foreign edit keeps the UID, that Outlook and the Proton Mail app keep the Message-ID while Apple Mail mints a new one, and that `X-Mailypoppins-Draft-Id` never survives; Message-ID first, then a single draft with the same recipients, subject and reply target, is the only rule the probe supports that also catches an Apple Mail edit, and a wrong guess costs a duplicate, never a lost draft. A draft that links to nothing comes back as a second copy and the old file moves to `drafts/.retired/`, which the reconcile prunes after 30 days. Bridge keeps the Message-ID and the attachment, so the link holds on Proton and its switch defaults to on like TUM's; a reply draft sent from the Proton Mail app loses its threading because Bridge drops `In-Reply-To`, while mp's own sends keep it from the local file. An agent may delete the probe's leftover drafts from the TUM and Proton Drafts and Deleted Items folders once it has listed them in the ticket. Ticket [#0143](docs/tickets/0143-server-drafts.md).
Rejected: Message-ID only (every Apple Mail edit would produce a second copy); the mirror off by default for Proton; keeping retired files for 7 days or for ever.
Source: decisions page `.sync/2026-10-22-mailypoppins-open-decisions.html`, section 2, answered 2026-10-22; probe dumps in `.sync/draft-probe/`.
Status: accepted

## 2026-10-22 - A newly discovered user folder syncs every tick like Inbox, and discovery lands before the server drafts

Why: a counts-only default would bring back the Spesen problem, local search missing every message of a folder never opened; the cost is five more folder visits per tick on TUM and some disk. Junk, Trash and Proton's label folders get counts only, since the labels repeat messages that live in real folders; Exchange's calendar and contact folders and Proton's All Mail and Starred are ignored. The catalogue and the folder roles come first, and the server drafts reuse the `drafts` role. Ticket [#0141](docs/tickets/0141-mailbox-auto-discovery.md).
Rejected: counts only for user folders until pinned in the config (the ticket's first proposal); the server drafts first with the Drafts folder found by name.
Source: decisions page `.sync/2026-10-22-mailypoppins-open-decisions.html`, section 3, answered 2026-10-22.
Status: accepted

## 2026-10-22 - Named profiles follow four design points and show the active profile in both clients

Why: a profile together with an explicit config or data folder variable is refused with an error naming both; the login service stays for the default profile, and a profile's daemon starts on demand and stops once its last window closes; a profile name holds letters, digits, `-` and `_`, at most 12 characters, because the Mac's socket path limit leaves 12 characters beside the user name; the default profile keeps its folders with no migration. The active profile shows at the foot of the desktop sidebar, in the window title and in the TUI status line, which prevents archiving real mail while believing to be in the demo; the sidebar foot carries it on the Mac, where the title bar is hidden. Plane PERSO-176.
Rejected: showing the profile nowhere (a demo video would show nothing unusual, at the cost of that mistake); a setting to hide it.
Source: decisions page `.sync/2026-10-22-mailypoppins-open-decisions.html`, section 4, answered 2026-10-22.
Status: accepted

## 2026-10-22 - v0.12.0 is tagged now, and auto-update stage 2 is the quiet daemon restart plus two stage-1 fixes

Why: the first in-app update needs a second release with the updater in it, and over 230 commits of features since v0.11.0 make a minor version. Stage 2 restarts the old daemon quietly after an update, stops the release job from marking a prerelease tag as the latest release, and has a manual check after Later say that a restart is pending. Ticket [#0139](docs/tickets/0139-desktop-auto-update.md), Plane PERSO-102 and PERSO-107.
Rejected: v0.11.1 (the number follows the content); tagging after the account picker and the prefix wait land; the Settings toggle that installs updates silently at quit (it replaces the app unseen, so it waits until a few manual updates have gone through cleanly).
Source: decisions page `.sync/2026-10-22-mailypoppins-open-decisions.html`, section 5, answered 2026-10-22.
Status: accepted

## 2026-10-22 - Checks that need a desktop window start with an agent's pre-check in a browser, then a checklist of native-only items for Sylvain

Why: the desktop app runs in a browser preview on the fixture backend, where an agent can press keys and click, so it takes every check that does not need the native shell; what is left (native file pickers, the terminal editor, logging out for the login service, Gatekeeper, the release install) goes on a one-page checklist Sylvain works through, and each ticket closes as its items come back ticked.
Rejected: Sylvain running every check from the checklist (the page's recommendation; it leaves work an agent can do); granting the terminal the macOS Accessibility permission for an unattended agent (it could control every app meanwhile).
Source: decisions page `.sync/2026-10-22-mailypoppins-open-decisions.html`, section 6, answered 2026-10-22.
Status: accepted

## 2026-10-22 - The server half of `mp search` runs on the daemon's server search, which takes over the PERSO-175 semantics

Why: it is the last command off the daemon path, and one server search serves the CLI, the TUI and the desktop. The CLI keeps its three visible behaviours, fed by the daemon: a failed mailbox prints the same `Search in <mailbox> failed` line, at the end of the search; `--mailbox` accepts any folder the server lists, the rule discovery brings; on plain IMAP the `has:attachment` notice stays on stderr, in the result header and in the JSON. The limit counts results across every mailbox and the answer says when it was cut (PERSO-175), and the CLI's own IMAP search path goes. Parity row LST-06.
Rejected: the per-mailbox failure line printed as the search runs; refusing a folder the account does not declare; dropping the `has:attachment` notice.
Source: decisions page `.sync/2026-10-22-mailypoppins-open-decisions.html`, section 9, answered 2026-10-22.
Status: accepted

## 2026-10-22 - The six daemon leftovers are all built or removed, none parked

Why: `spikes/ipc-bench` is deleted, since the last phase re-ran its workloads through the product binaries; a daemon method answers whether each password and token is present, never the value, so `mp config show` stops reading the keychain in the CLI; `-s`, `--no-signature` and `-A` are scoped to the commands they mean something for; the CLI setup wizards move onto the daemon, as the desktop wizard already has; `mp fetch` is retired as a duplicate of `mp search --mailbox`; the five daemon gaps the desktop works around are built (a `signature.removed` event and the remaining signature methods, a vCard method, a contacts-changed event, a visible notice when contact search refuses a rebuild) and the desktop workarounds go.
Rejected: leaving the CLI wizards in the client because they run once per account; keeping `mp fetch` because it already runs through the daemon; parking the five gaps until one causes a visible bug (all three were the page's recommendations).
Source: decisions page `.sync/2026-10-22-mailypoppins-open-decisions.html`, section 10, answered 2026-10-22.
Status: accepted

## 2026-10-22 - The codebase is kept clean as it is worked on, and every mailbox operation goes through the daemon with one set of calls for all clients

Why: something sub-optimal noticed in passing is fixed in the same pass instead of parked as a leftover, which is what grew the daemon follow-up lists; the CLI, the TUI and the desktop make the same daemon calls and offer the same capabilities, so a fix or a feature lands once. Both are stated in `AGENTS.md`, and the second also in `docs/architecture.md`, "Project invariants".
Rejected: parking follow-ups in BACKLOG.md until one causes a visible bug.
Source: decisions page `.sync/2026-10-22-mailypoppins-open-decisions.html`, comments, 2026-10-22.
Status: accepted
