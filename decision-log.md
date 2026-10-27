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

Why: the 1.2 s wait was too short, so a late second key fired its flat action (`a` after `c` archived instead of replying to all); 5 s leaves room for a slow second key while an accidental prefix still lapses on its own. Ticket #0148, plan `docs/plans/chord-timeout.md`.
Rejected: no timeout by default (an accidental prefix keeps the which-key overlay up until Escape; it stays a choice in Settings); 3 s (less room for the second key).
Source: chat with Sylvain, 2026-10-22.
Status: superseded by the 2026-10-22 entry "The desktop prefix chord never times out"

## 2026-10-22 - The TUI sidebar keeps showing only the active account; collapsing the other accounts is desktop only

Why: the TUI sidebar never lists the other accounts, so the request is already met there, and the `ga` picker covers switching. Ticket #0147, plan `docs/plans/account-picker.md` (Decision 6).
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

Why: a plain list with the digits 1 to 9 and no filter field keeps `j` and `k` as navigation; each row shows new mail as the inbox unread count in the desktop and as a dot in the TUI, which keeps no per-account unread count; a desktop account expanded or collapsed by hand stays so until the app quits, and every start shows only the selected account expanded; in the desktop sidebar `j` and `k` skip collapsed accounts and `ga` is the keyboard path to another account, and picking the current account there expands it again after a hand collapse, since its header takes no key; a row of an account that is not serving says so in both pickers (the runtime state in the desktop, `not ready` in the TUI, which has one flag for opening and blocked); the unreachable `Ctrl+1..9` account jump leaves the TUI code. Ticket #0147, plan `docs/plans/account-picker.md`.
Rejected: a type-to-filter field (it would swallow `j` and `k`); a TUI unread count per account (new state for a cosmetic gain); remembering the expand state across restarts; a cursor stop on every account header with Enter to toggle it (a second keyboard model for the picker's job).
Source: decisions page `.sync/2026-10-22-mailypoppins-open-decisions.html`, section 1, answered 2026-10-22.
Status: accepted, amended by the 2026-10-22 entry "Both account pickers say opening, blocked or sync failed"

## 2026-10-22 - The desktop prefix chord never times out, as in the TUI

Why: a pending prefix waits for its second key with no timer and no setting, so `c`, a pause of any length, then `a` replies to all in both clients; Escape cancels the prefix and does nothing else, a click drops it so an abandoned chord never resolves with a much later key, and the which-key popup appears at once. Ticket #0148, plan `docs/plans/chord-timeout.md`.
Rejected: a 5 s default with four presets in Settings (the earlier entry of the same day; two behaviours between the clients and a setting changed once); a show delay before the popup.
Source: decisions page `.sync/2026-10-22-mailypoppins-open-decisions.html`, section 1, answered 2026-10-22.
Status: accepted

## 2026-10-22 - Server drafts link by Message-ID, then by a single matching draft, and the mirror is on for Proton too

Why: the probe of 2026-10-20 showed that no foreign edit keeps the UID, that Outlook and the Proton Mail app keep the Message-ID while Apple Mail mints a new one, and that `X-Mailypoppins-Draft-Id` never survives; Message-ID first, then a single draft with the same recipients, subject and reply target, is the only rule the probe supports that also catches an Apple Mail edit, and a wrong guess costs a duplicate, never a lost draft. A draft that links to nothing comes back as a second copy and the old file moves to `drafts/.retired/`, which the reconcile prunes after 30 days. Bridge keeps the Message-ID and the attachment, so the link holds on Proton and its switch defaults to on like TUM's; a reply draft sent from the Proton Mail app loses its threading because Bridge drops `In-Reply-To`, while mp's own sends keep it from the local file. An agent may delete the probe's leftover drafts from the TUM and Proton Drafts and Deleted Items folders once it has listed them in the ticket. Ticket PERSO-139.
Rejected: Message-ID only (every Apple Mail edit would produce a second copy); the mirror off by default for Proton; keeping retired files for 7 days or for ever.
Source: decisions page `.sync/2026-10-22-mailypoppins-open-decisions.html`, section 2, answered 2026-10-22; probe dumps in `.sync/draft-probe/`.
Status: accepted

## 2026-10-22 - A newly discovered user folder syncs every tick like Inbox, and discovery lands before the server drafts

Why: a counts-only default would bring back the Spesen problem, local search missing every message of a folder never opened; the cost is five more folder visits per tick on TUM and some disk. Junk, Trash and Proton's label folders get counts only, since the labels repeat messages that live in real folders; Exchange's calendar and contact folders and Proton's All Mail and Starred are ignored. The catalogue and the folder roles come first, and the server drafts reuse the `drafts` role. Ticket #0141.
Rejected: counts only for user folders until pinned in the config (the ticket's first proposal); the server drafts first with the Drafts folder found by name.
Source: decisions page `.sync/2026-10-22-mailypoppins-open-decisions.html`, section 3, answered 2026-10-22.
Status: accepted

## 2026-10-22 - Named profiles follow four design points and show the active profile in both clients

Why: a profile together with an explicit config or data folder variable is refused with an error naming both; the login service stays for the default profile, and a profile's daemon starts on demand and stops once its last window closes; a profile name holds letters, digits, `-` and `_`, at most 12 characters, because the Mac's socket path limit leaves 12 characters beside the user name; the default profile keeps its folders with no migration. The active profile shows at the foot of the desktop sidebar, in the window title and in the TUI status line, which prevents archiving real mail while believing to be in the demo; the sidebar foot carries it on the Mac, where the title bar is hidden. Plane PERSO-176.
Rejected: showing the profile nowhere (a demo video would show nothing unusual, at the cost of that mistake); a setting to hide it.
Source: decisions page `.sync/2026-10-22-mailypoppins-open-decisions.html`, section 4, answered 2026-10-22.
Status: accepted

## 2026-10-22 - v0.12.0 is tagged now, and auto-update stage 2 is the quiet daemon restart plus two stage-1 fixes

Why: the first in-app update needs a second release with the updater in it, and over 230 commits of features since v0.11.0 make a minor version. Stage 2 restarts the old daemon quietly after an update, stops the release job from marking a prerelease tag as the latest release, and has a manual check after Later say that a restart is pending. Issue PERSO-209 (#0139), with PERSO-102 and PERSO-107.
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
Rejected: parking follow-ups in the backlog until one causes a visible bug.
Source: decisions page `.sync/2026-10-22-mailypoppins-open-decisions.html`, comments, 2026-10-22.
Status: accepted

## 2026-10-22 - Both account pickers say opening, blocked or sync failed (amends the account picker entry)

Why: the TUI reads the daemon's account state from the bootstrap and from `account.state_changed`, so its picker tells an opening account from a blocked one as the desktop's does, where it had one `not ready` flag for both; both pickers also mark an account whose last sync failed, the TUI from its own sync outcomes and from the bootstrap's `sync_health`, so a failure from before the TUI started shows too.
Rejected: keeping the single `not ready` tag (the two pickers would say different things about the same account).
Source: the review of the 2026-10-22 cleanup batch, under the same-capabilities principle above.
Status: accepted

## 2026-10-22 - The trash and junk folders show the server's counts and download their newest 50 messages each tick, on IMAP and Graph alike

Why: a message another client moves to the trash is the newest there, so a 50-message tail lets the move land as a row in the pass that prunes its source row (#0072), without downloading the folder's history; the counts come from `STATUS` on IMAP and from the folder tree on Graph. The tail is the newest 50 of the folder whether the store holds them or not, and it never owes a backlog, so back to back passes do not drain the whole folder. Label folders download nothing.
Rejected: counts only (a moved message would lose its row); a full sync of the trash and junk folders; on Graph, capping the new messages at 50, which took the next older 50 on every pass.
Source: the #0141 implementation and its review, orchestrator brief of 2026-10-22; Sylvain may veto any of them.
Status: accepted

## 2026-10-22 - The server's Drafts folder is catalogued with the drafts role and hidden until #0143 mirrors it

Why: the sidebar's Drafts row is the local drafts directory, and listing the server copy beside it would show two Drafts for the same thing; #0143 reuses the role when it adds the reconcile.
Rejected: syncing the server Drafts as a received folder now.
Source: the #0141 implementation and its review, orchestrator brief of 2026-10-22; Sylvain may veto any of them.
Status: accepted

## 2026-10-22 - Opening a counts-only folder does not download it in full

Why: for a Proton label every message would be stored a second time, and the trash and junk tails already keep a moved message's row; `mp sync --mailbox <folder>` downloads one in full when it is wanted.
Rejected: a full fetch on open, which the ticket suggested.
Source: the #0141 implementation and its review, orchestrator brief of 2026-10-22; Sylvain may veto any of them.
Status: accepted

## 2026-10-22 - A folder whose name would read back as a role key or the local Drafts is ignored and refused everywhere

Why: a plain `Sent` beside the `\Sent` folder, or a server `Drafts`, would file rows under the role's key and show them in the wrong mailbox; `[accounts.mailboxes.<role>]` maps such a folder when it is the role's.
Rejected: a renamed store key for the colliding folder, which no selector a person types would reach.
Source: the #0141 implementation and its review, orchestrator brief of 2026-10-22; Sylvain may veto any of them.
Status: accepted

## 2026-10-22 - A folder policy override is `[[accounts.mailboxes.override]]` with `match` and `policy`, first match wins

Why: `match` is a case-insensitive glob over the server name where `*` crosses the hierarchy delimiter, `policy` is `sync`, `status-only` or `ignore`, and the order of the entries is the precedence; an override cannot unpin a configured folder or sync one that cannot be selected. The CLI and desktop setup wizards write the same entries.
Rejected: a per-folder table keyed by name, which a glob cannot express.
Source: the #0141 implementation and its review, orchestrator brief of 2026-10-22; Sylvain may veto any of them.
Status: accepted

## 2026-10-22 - A mailbox name matches a store key, then a label, then a server name, in every client

Why: matching every name of one row before the next let the `\Trash` folder, labelled `Trash` and sorted first, capture a pinned extra called `Trash`. One function (`mp_protocol::state::pick_mailbox`) serves the daemon, `mp`, the TUI and the search targets.
Rejected: label first, the old order.
Source: the #0141 implementation and its review, orchestrator brief of 2026-10-22; Sylvain may veto any of them.
Status: accepted

## 2026-10-22 - On Graph a config name for a well-known folder designates the localized folder the listing carries

Why: Graph resolves `Inbox` or `Sent Items` to the well-known folder whatever the mailbox's language, so an English config on a German mailbox pinned a second folder and synced `Posteingang` twice. Graph display names are never decoded as modified UTF-7.
Rejected: matching config names by display name alone.
Source: the #0141 implementation and its review, orchestrator brief of 2026-10-22; Sylvain may veto any of them.
Status: accepted

## 2026-10-22 - The setup wizards stop pinning extra folders and ask for the drafts, trash and junk folders and the overrides

Why: the daemon syncs every folder the server lists, so an `extra` entry only freezes a folder's key and keeps it synced against a later override. Each role folder is left empty for the one the server marks. `mp config show` lists every discovered folder with its policy, ignored ones included.
Rejected: keeping the extra-folder prompt for familiarity.
Source: the #0141 implementation and its review, orchestrator brief of 2026-10-22; Sylvain may veto any of them.
Status: accepted

## 2026-10-22 - A renamed Graph folder keeps its rows, and IMAP rename detection waits for a check of the live servers

Why: Graph's folder id survives a rename, so the listing stores it and a new name with a known id moves the folder's rows, cursor and delta token to the new key; a pinned old name is left alone and logged. IMAP has no folder id; its detection by UIDVALIDITY and Message-ID overlap is planned in `docs/plans/mailbox-discovery.md` and needs a rename on TUM and Proton first to learn whether each keeps UIDVALIDITY.
Rejected: building the IMAP half on an assumption about the servers.
Source: the #0141 implementation and its review, orchestrator brief of 2026-10-22; Sylvain may veto any of them.
Status: accepted


## 2026-10-23 - The dark theme's warning colour is #FF7212

Why: the previous orange failed the contrast check against the grey panels; #FF7212 passes it with the panels unchanged, in the TUI and the desktop app.
Rejected: keeping the old orange and darkening the grey panels instead.
Source: Sylvain, chat of 2026-10-23.
Status: accepted

## 2026-10-23 - A draft's approval travels to mp on the other machine through its server copy

Why: a draft approved on the Mac should be sendable from the home server without approving it twice.
The status rides inside the draft file the copy carries, so it is taken only from a copy mp pushed and nobody edited since.
A pull or an import takes it only when the carried source reproduces the pushed file, and every attachment is here or carried by the copy; an attachment file that is here must hold the bytes of the copy's part of the same name.
An edit in Outlook, Apple Mail or the Proton app demotes the draft to `status: draft` on both machines.
On Proton it does not travel, because Bridge drops the headers that carry the file and the copy's HTML holds no validated status.
Ticket PERSO-139.
Rejected: approval staying local (the build's provisional default, never logged as a decision); a status header of its own, which a copy mp never wrote could carry too; the status in the HTML mark for Proton, which nothing validates and which would approve a body converted from HTML.
Source: Sylvain, chat of 2026-10-23.
Status: accepted

## 2026-10-23 - Microsoft 365 accounts always go through Microsoft Graph, and the OAuth2 sign-in over IMAP and SMTP is retired

Why: one way to reach Microsoft 365 keeps the design simple, and no account uses the retired mode (TUM is password IMAP on the on-premises LRZ Exchange, EVOQS is Graph, Proton, OVH and Gmail are password accounts, checked in both machines' configs).
The XOAUTH2 paths in IMAP and SMTP, the IMAP and SMTP scopes and the wizards' Microsoft 365 IMAP preset are removed; the Graph sign-in, its token cache and the Entra app stay.
Password IMAP and SMTP stay for every server, on-premises Exchange included.
A config that still says `auth_method = "oauth2"` loads, and only that account is blocked with a sentence naming both ways out: `auth_method = "graph"` for Microsoft 365, a password for on-premises Exchange.
Rejected: keeping the mode behind a flag; failing the whole config on the retired value, which would stop every other account.
Source: Sylvain, chat of 2026-10-23.
Status: accepted

## 2026-10-23 - A forward from mp is laid out as Outlook writes it

Why: a forward should read like one written in Outlook, which Sylvain uses at TUM; his own Outlook mobile forward on the TUM account is the reference.
The subject takes `FW: `, and a subject that already opens with `FW:`, `Fwd:` or `WG:` keeps its prefix.
Above the original stand an empty line and the signature.
The original opens with a block under a 1pt solid #B5C4DF top border with a little top padding, holding bold English labels one per line: `From:`, `Sent:`, `To:`, `Cc:` (left out when the original had none) and `Subject:`; a blank line and the original body follow.
The date label is `Sent:`, which Outlook desktop and Outlook on the web write in English; Outlook mobile writes `Date:` in the same bordered block.
The date reads as Outlook writes it, such as `Wednesday, October 21, 2026 8:12 PM`, in the local time zone of the machine that writes the draft.
The plain-text part carries the same labels, one per line, without bold.
The block is one `parse::draft_reply_boundary` already takes for the start of a quote, so a copy edited in Outlook imports with the original below the quote marker.
Rejected: Gmail's `Fwd:` prefix and its `---------- Forwarded message ----------` header, mp's layout until now; Outlook mobile's `Date:` label.
Source: Sylvain, chat of 2026-10-23.
Status: accepted

## 2026-10-23 - A forward as attachment carries the signature alone in its body

Why: the attached `.eml` already holds the whole message, and Outlook writes a forward as attachment with an empty body above the signature.
The draft of `mp forward --as-attachment`, of `cF` in the TUI and of Forward as attachment in the desktop app holds the signature and nothing else: no quote marker, no header block, no companion HTML.
Its subject takes `FW: ` like every forward.
Rejected: the quoted body of an inline forward, which PERSO-174 shipped and which repeated the attachment's content.
Source: Sylvain, chat of 2026-10-23.
Status: accepted

## 2026-10-25 - The draft_sync row records each server copy's Message-ID and restores it into a file that lost it

Why: a draft file rewritten without `server_message_id` was taken for sent elsewhere, retired, and its send skipped the claim that keeps the other machine from sending it too (PERSO-192).
The row is the authority the file's edits cannot reach: the push, the reconcile and the daemon's send put the key back from it before reading the draft, and a row with a copy but no recorded Message-ID refuses the send until the next reconcile learns it.
The column is added additively; a row an earlier build wrote learns the Message-ID from the file's key, or from the copy its UID names.
Rejected: fetching the copy by the row's UID and reading its `Message-ID` header on every recovery, which costs an IMAP round trip each time and fails offline.
Source: Sylvain, chat of 2026-10-25.
Status: accepted

## 2026-10-26 - Open work lives in Plane, project PERSO, module Mailypoppins; BACKLOG.md and docs/tickets/ are retired

Why: Plane is the single issue tracker for every code repo Sylvain maintains, so the repo stops keeping a second, plain-text one; each open ticket became a PERSO issue whose title keeps its number (PERSO-139 is "Two-way server drafts (#0143)"), the daemon follow-up lists of the backlog became one issue per phase, and the settled daemon-migration deferrals moved to the "Settled deferrals" section of `docs/plans/native-gui.md`.
Docs and comments cite an issue by its identifier (`PERSO-139`); a four-digit number (`#0143`) names a retired ticket, which git keeps (last in the tree at `ed93905f`).
Rejected: keeping BACKLOG.md as an index beside Plane (two trackers drift); moving the done tickets into Plane too (git history holds them).
Source: Sylvain, decision of 2026-10-26; migration tracked in PERSO-194.
Status: accepted

## 2026-10-27 - A draft the daemon creates gets its server_message_id at creation, not at its first push

Why: every client opens a new draft in an editor at once, and the first push seconds later wrote the key under the editor's open buffer, so saving dropped the key and not saving dropped the edits (PERSO-231).
The daemon writes the key in the one helper every creation method ends in, before the path goes out, on an account that mirrors; from then on no push writes a draft mp created, and a file written outside mp still gets its key at its first push.
A new draft therefore reads `pending` rather than `local` until its first push, and a send before that push claims on the server, which finds nothing and goes ahead; with the Drafts folder unreachable that send is refused, as for any mirrored draft, since a draft with no row cannot be told apart from one whose row a store rebuild dropped while its copy sits on the server.
Rejected: keeping the key in the `draft_sync` row only, which a store rebuild drops; holding the write while an editor has the file open, which depends on the editor and misses the desktop app.
Source: Sylvain, chat of 2026-10-27.
Status: accepted
