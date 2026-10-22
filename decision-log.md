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
Status: accepted

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
