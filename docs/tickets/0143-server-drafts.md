---
id: 0143
title: Mirror drafts to the server Drafts folder, two-way
type: feature
priority: next
status: open
created: 2026-10-13
---

Sylvain decided on 2026-10-13 that drafts converge across mp on the Mac, mp on the home server, Outlook and Apple Mail, through the server's Drafts folder.
This replaces decision 2 of [data-access-layer.md](../plans/data-access-layer.md), which kept drafts local-only.

## Problem

A draft written in mp exists in one drafts directory on one machine.
It never reaches the server, so the home server's mp, Outlook and Apple Mail cannot see it, and a draft started in Outlook never reaches mp as a draft.
Outlook and Apple Mail already converge with each other through the server's Drafts folder: IMAP `Drafts` with the `\Draft` flag, and Graph's `/me/mailFolders/drafts` messages with `isDraft`.

### Current behaviour

- The drafts directory `<account_dir>/drafts/` is the only local truth, and the `drafts` table is a derived index over it (`docs/architecture.md:11-12`).
- The server `Drafts` folder, when it is synced at all, lands in the received namespace as read-only mail; the local pseudo-mailbox `drafts` (`DRAFTS_MAILBOX`, `crates/mp-core/src/selector.rs:76-80`) is a separate thing that never touches it.
- Both accounts carry a server `Drafts` folder that collides with that pseudo-mailbox, which [#0141](0141-mailbox-auto-discovery.md) records under "Store-key collisions".
- The only IMAP APPEND in the tree is the outbox's Sent copy (`ImapSentMailbox::append`, `src/imap_client/sent.rs:93-119`).
- A fully sent draft is deleted from `drafts/` (`settle_sent_draft`, `src/draft.rs:165-204`; `docs/architecture.md:299`), and `draft.discard` deletes the file and its HTML companion (`remove_draft_files`, `crates/mp-core/src/draft.rs:1954-1969`).
- Both accounts are IMAP: `tum` on TUM Exchange with OAuth2, and `proton` through Proton Bridge.
  The Graph backend (`src/graph.rs`) has no account and no draft method: it lists folders, fetches, moves, deletes (`src/graph.rs:2203`) and sends (`src/graph.rs:2015`).

## Proposal

Four slices, each its own commit series, each leaving the tree green.
Slice 0 is a probe whose findings choose the linking rule slices 1 and 2 build on.

### Slice 0, the probe

The probe answers what Outlook and Apple Mail do to a draft another client wrote.
No production code changes in this slice.

The probe is a cargo example, `examples/draft_probe.rs`, beside the existing `examples/mkfixture.rs`.
It links the root crate's library, loads the account from `config.toml` and opens its session through `open_imap_session` (`src/imap_client/mod.rs:191`), the path the daemon takes, OAuth2 for `tum` included.
It is not an `mp` subcommand, because the CLI is a daemon client and the engine-touch allow-list (`docs/architecture.md:155`) would have to admit an IMAP session in `src/main.rs` for a tool nobody ships.
It opens no store and takes no engine lock, which lets it run beside a live daemon.

Two subcommands:

- `cargo run --example draft_probe -- append <account>` APPENDs one draft to the account's Drafts folder with the `\Draft` flag.
  The draft is `multipart/mixed` around a `multipart/alternative` of `text/plain` and `text/html`, as `build_draft_message` (`src/send.rs:2757`) builds one, plus one small PDF.
  It carries a fresh `Message-ID`, `X-Mailypoppins-Draft-Id: <id>`, an `In-Reply-To` naming a real message of the account, a To, a Cc and a Bcc, and in its HTML the comment `<!-- mp:quote -->` (`QUOTE_MARKER`, `crates/mp-core/src/draft.rs:90`) and a signature sentinel pair.
  It prints the id.
- `cargo run --example draft_probe -- dump <account> <id>` prints, for every message in Drafts whose marker or Message-ID matches the id, and for every message that appeared since the APPEND: UID, UIDVALIDITY, FLAGS, INTERNALDATE, Message-ID, the marker header, In-Reply-To, References, Bcc, `BODYSTRUCTURE`, and the first 2 KiB of each text part.
  It also prints the server's `CAPABILITY` line and the Drafts folder's LIST attributes.
  Output goes to stdout and to `/var/tmp/mp-draft-probe-<account>-<id>.txt`; it carries probe content only.

The run, for each of `tum` and `proton`:

1. APPEND a draft and dump it.
2. Open it in Outlook, change a word in the body and add a recipient, save, close, dump.
3. Open the same draft in Apple Mail, change a word, save, close, dump.
4. APPEND a second draft, send it from Outlook, dump: the Drafts copy should be gone.
5. APPEND a third draft, delete it in Apple Mail, dump: expunged, flagged `\Deleted`, or moved to Trash.
6. Start a draft in Outlook and one in Apple Mail, dump both: the body MIME, the flags and the headers a foreign draft carries.

What each dump records:

- Whether the edit kept the UID or replaced the message under a new one.
- Whether the `Message-ID` survived the edit.
- Whether `X-Mailypoppins-Draft-Id` survived the edit.
- Whether `In-Reply-To`, `References` and Bcc survived.
- The body MIME after the edit: `text/plain` only, `text/html` only, or `multipart/alternative`, and what became of the attachment.
- Whether the HTML comments survived.
- Whether `\Draft` and `\Seen` are set.
- `UIDPLUS`, `CONDSTORE`, `SPECIAL-USE` and the Drafts folder's `\Drafts` attribute.

#### Running the probe

The commands run from the repository root on the Mac, once with `tum` and once with `proton` in place of `<account>`.
`append` prints the draft's id; `<id1>`, `<id2>` and `<id3>` stand for the ids of the three appends.
The probe draft is addressed to the account's own address in To, Cc and Bcc, so the send of step 4 reaches only Sylvain; `--to`, `--cc` and `--bcc` override that.
Each `dump` appends its output to `/var/tmp/mp-draft-probe-<account>-<id>.txt`, so one file holds the history of one draft across the steps.
`append` keeps its state and the appended bytes beside it, as `.json` and `.eml`.
Each dump also searches the account's Trash folder for the draft, the folder carrying `\Trash` or else the one named `Deleted Items` or `Trash`, so step 5 shows a move there.

1. `cargo run --example draft_probe -- append <account>`
   `cargo run --example draft_probe -- dump <account> <id1> --label "1 append"`
2. Edit the draft in Outlook (change a word, add a recipient, save, close), then:
   `cargo run --example draft_probe -- dump <account> <id1> --label "2 Outlook edit"`
3. Edit the same draft in Apple Mail (change a word, save, close), then:
   `cargo run --example draft_probe -- dump <account> <id1> --label "3 Apple Mail edit"`
4. `cargo run --example draft_probe -- append <account>`
   Send that draft from Outlook, then:
   `cargo run --example draft_probe -- dump <account> <id2> --label "4 sent from Outlook"`
5. `cargo run --example draft_probe -- append <account>`
   Delete that draft in Apple Mail, then:
   `cargo run --example draft_probe -- dump <account> <id3> --label "5 deleted in Apple Mail"`
6. Start a draft in Outlook and one in Apple Mail, save both, then:
   `cargo run --example draft_probe -- dump <account> <id3> --label "6 foreign drafts"`
   Every message that reached Drafts after the third append is listed as `since-append`.

### How the probe's outcome selects the linking rule

A link ties a local draft file to its server copy across an edit made elsewhere.
mp's own replace always keeps the link, because mp writes the same Message-ID and marker into every copy it APPENDs.
The question is only what survives an edit in Outlook or Apple Mail.
The rule is a chain tried in order, and the probe decides which links stay in the chain per server:

1. UID.
   Selected if the edit keeps the UID.
   IMAP's message immutability (RFC 3501) makes this unlikely, but an Exchange gateway may rewrite in place.
   With a kept UID a change is detected by `MODSEQ` where `CONDSTORE` is offered, and by a hash of the body otherwise.
2. Message-ID.
   Selected if the edit gives the message a new UID but keeps its `Message-ID`.
   The link is the `server_message_id` recorded in the draft's frontmatter.
3. Marker header.
   Selected if the `Message-ID` changes but `X-Mailypoppins-Draft-Id` survives.
   The link is the draft's own `id:`.
4. Fallback, when neither survives.
   A server draft that appeared in the same sync as the linked copy vanished is matched on its `In-Reply-To` target, its recipient set and its subject.
   Exactly one candidate is a link; none or several is no link.

A broken link costs a duplicate and never a loss: the local file reads as "server copy gone" and is moved to `drafts/.retired/`, and the edited server copy is imported as a foreign draft (slice 2).
The probe results also decide whether Proton gets the mirror at all: if Bridge rewrites the MIME so that no link survives and the HTML does not convert back, the account's switch below stays off for Proton.

### Probe results

Not run yet.
The findings go here when the probe has run, one subsection per account, with the dump files named by path.

### Slice 1, mp-owned drafts two-way

#### The frontmatter keys

`EmailFrontmatter` (`crates/mp-core/src/types.rs:310`) carries today `id`, `to`, `cc`, `bcc`, `subject`, `status`, `from`, `reply_to`, `attachments`, `sent_at`, `sent_via`, `message_id`, `in_reply_to`, `forwarded_from`, `date`, `signature` and `event`; `parse_email_draft` (`crates/mp-core/src/draft.rs:1700`) deserialises it whole.
`message_id` is the Message-ID of the sent message, written by `mark_draft_sent` (`crates/mp-core/src/draft.rs:1899`), and is not reused here.

Slice 1 adds six optional keys, each `skip_serializing_if = "Option::is_none"` so a draft without them parses unchanged:

- `server_message_id`: the Message-ID every server copy of this draft carries, minted once by mp when the draft is first pushed and kept for its life.
  It differs from the Message-ID the send mints (`src/send.rs:2829-2832`), so the sent message and the draft are never confused by a Message-ID search.
- `server_mailbox`: the server folder the copy lives in.
- `server_uid` and `server_uidvalidity`: the UID of the current copy and the folder's UIDVALIDITY when it was recorded, because a UID means nothing without it.
- `server_synced_at`: RFC 3339 time of the last push or pull that converged.
- `server_synced_hash`: a hash of the file's content with the six `server_*` keys removed, taken at that convergence.
  A file whose hash differs has changed locally since; a file whose hash matches has not.

A seventh key, `conflict_of: <id>`, marks a conflict copy (below).

The daemon writes these keys with `rewrite_frontmatter_scalars` (`crates/mp-core/src/draft.rs:1540`), the surgical writer `mark_draft_sent` uses, which leaves every other byte of the file as it was.
The watcher sees that write as a change and reparses; the hash still matches and no push follows.

#### When a push is owed

- The draft watcher (`src/daemon/watch.rs`, the one-second poll with a 300 ms settle, `:52-55`) settles a changed file into `WatchEvent::DraftChanged`.
  `src/daemon/runtime/watcher.rs` is the IDLE watcher for received mail and takes no part in this.
- The watcher opens no store and takes no engine lock (`src/daemon/watch.rs:606-610`), and that stays true.
  Its loop, `spawn` (`src/daemon/watch.rs:616`), gains the `RuntimeTable` and hands each settled change of a draft whose hash differs from `server_synced_hash` to that account's runtime, which enqueues the push and calls `request_drain` (`src/daemon/runtime/account.rs:463`).
- The `draft.*` commands that write a file (`draft.create`, `draft.reply`, `draft.forward`, `draft.create_from_message`, `draft.attach`, `draft.detach`, `draft.approve`, `draft.demote`) need no hook of their own: the watcher notices their writes like any other, as it already does for `draft.approve` (`docs/daemon-protocol.md:748`).
- A draft with `conflict_of:` set is never pushed.
- An account whose switch is off pushes nothing (see "The per-account switch").

#### The push as a queued op

The push is a `pending_ops` row (`src/pending_ops.rs`), drained by the account's drainer after `DRAIN_DEBOUNCE` (`src/daemon/runtime/drainer.rs:76`, 1.5 s) like a queued archive.
`ServerOp` (`src/ops.rs:81`) gains two kinds:

- `PushDraft { draft_id }`: render the file as it is at drain time, APPEND it, retire the previous copy.
- `DeleteDraft { mailbox, message_id, uid, uidvalidity }`: remove every copy of one draft from the server folder.

`PushDraft` names the draft and carries no bytes.
The drain reads the file when it runs, which makes a burst of edits one APPEND of the last state.
The enqueue is a no-op when a queued `PushDraft` for the same id already exists.
It also keeps the blob invariant trivially: there is no blob to write before the transaction.
`target_message_id` is NULL, because the op addresses a file and no `messages` row.

The drain of a `PushDraft`:

1. Parse the file; a file that is gone or does not parse retires the row with nothing done.
2. Build the server copy (below).
3. APPEND it to `server_mailbox` with `(\Draft \Seen)`.
   A first push mints `server_message_id` and resolves the folder.
4. Recover the new UID with `UID SEARCH HEADER MESSAGE-ID`, as `ImapSentMailbox::append` does (`src/imap_client/sent.rs:108-118`), because `async-imap` 0.11 drops `APPENDUID` (`src/imap_client/sent.rs:9-20`).
   Every copy shares the Message-ID, so the new copy is the highest UID the search returns.
5. Delete the previous copy by UID: `UID STORE +FLAGS (\Deleted)` and `UID EXPUNGE` when the server offers `UIDPLUS`.
   Without `UIDPLUS` the old copy is only flagged `\Deleted`, since a plain `EXPUNGE` would also expunge whatever another client flagged in that folder; the reconcile ignores flagged copies.
   The existing `delete_email_on_server` (`src/imap_client/ops.rs:57`) finds its target by Message-ID, which here matches every copy, so it is not reused for this step.
6. Write `server_uid`, `server_uidvalidity`, `server_synced_at` and `server_synced_hash` into the file.

APPEND comes before the delete, so a crash between them leaves two copies rather than none, and the reconcile removes the older one.
A refusal follows the queue's backoff (`crate::outbox::backoff_secs`) and parks the row `failed` past its budget with `Rollback::None` (`src/pending_ops.rs:97`): the local file is the truth, so there is nothing local to undo.

`pending_ops` is a cache table that a store rebuild drops (`docs/architecture.md:228`).
A lost `PushDraft` is re-derived at the next reconcile, because the file's hash still differs from `server_synced_hash`.
The server identity lives in the frontmatter for the same reason: the store may lose it, the file may not.

#### Building the server copy

`build_draft_message` (`src/send.rs:2757`) cannot be reused as it stands: it refuses a draft that is not approved (`:2764-2770`), refuses one without recipients (`:2814-2816`), mints a fresh Message-ID per build (`:2829-2832`) and lets lettre drop the Bcc header.
Slice 1 splits its MIME assembly (`:2866-2947`: the HTML from `markdown_to_html`, the companion `.html` quote, the alternative part, the attachments) into a shared function, and adds `build_server_draft` beside it, which:

- accepts any status and an empty recipient list or subject, since a draft is allowed to be unfinished;
- stamps `server_message_id` and `X-Mailypoppins-Draft-Id: <id>`;
- keeps Bcc with lettre's `keep_bcc()`, because a draft's Bcc is part of what the user wrote;
- writes `In-Reply-To` and `References` from `in_reply_to:`, which Outlook and Apple Mail need to show a reply draft as a reply;
- keeps the signature sentinels and `QUOTE_MARKER` as HTML comments in the HTML part, for a pull to restore, if the probe shows they survive.

The send path emits no `In-Reply-To` today either: outside its tests, `src/send.rs` reads `in_reply_to:` only to flag the source after a send (`source_to_flag`, `:2309-2313`).
That is a gap of its own and listed under "Out of scope".

#### Attachments

- Pushing: the `attachments:` entries resolve through `resolve_attachment_paths` (`src/send.rs:2132`) against the draft's directory, exactly as a send resolves them, and each file is MIME-encoded into the server copy.
  A missing file is skipped with a warning in the copy, where a send would fail; the draft stays editable.
- Pulling: a server copy's attachments that the local file does not list are written as files under `stable_attachments_dir(account_dir, server_message_id)` (`crates/mp-core/src/parse.rs:870`) and listed in `attachments:`.
  That is where a forward already writes its source's attachments (`src/draft.rs:41-66`, `crates/mp-core/src/draft.rs:416-436`), so a draft's attachments stay plain paths in both directions.
  An attachment the server copy no longer carries is dropped from `attachments:` and its file is left in place.
  Inline images (`cid:`) are written the same way and their placement in the body is lost; the probe's dumps say how often that matters.

#### The reconcile, after each sync of the Drafts folder

The server Drafts folder is ingested by the normal sync, as any synced mailbox is.
The conversation list of [#0142](0142-conversation-list.md) excludes the rows of the `drafts`-role mailbox from its rows, counts and `thread_messages`, and shows the local file as the one draft member of a conversation, so a mirrored draft never appears twice.
After the ingest, inside the runtime turn and under the engine lock (`src/engine_lock.rs`), a reconcile compares the folder's rows against the drafts directory.

Ingest records `X-Mailypoppins-Draft-Id` in a new nullable `messages.draft_marker` column for that folder, because the retention sweep (#0060) may evict the raw blob the reconcile would otherwise read it from.
The Drafts folder is exempt from body eviction, because a pull needs the body.

For each local draft with a `server_message_id`:

- The linked server copy is unchanged (same UID, or the rule's equivalent) and the file's hash matches: nothing to do.
- The file's hash differs and the server copy is unchanged: a push is owed; enqueue `PushDraft` if none is queued.
- The server copy changed and the file's hash matches: pull.
- Both changed since `server_synced_at`: conflict.
- No server copy links to it: the copy was sent or deleted elsewhere; retire the file.
- Two or more copies link to it, which a crash between APPEND and delete or a concurrent push from the other machine leaves behind: the newest by UID is the copy; an older copy whose body equals a version mp pushed is deleted, and any other older copy is pulled as a conflict copy.

For each server copy carrying a marker that names no local file and no file in `drafts/.retired/`: it is a draft the other machine's mp pushed, and it is written as a new local file under that `id:` with its server keys.
Without this rule the Mac and the home server would not converge in slice 1, since each would only ever see its own drafts.

The latency is the scheduler's quick tick (`imap.sync_interval_secs`), because the IDLE watcher hears only INBOX (`src/daemon/runtime/watcher.rs:21-28`).
That is the latency Outlook and Apple Mail show between each other on IMAP as well.

#### Pull

A pull rewrites the body and the headers of the local file from the server copy and keeps every frontmatter key the server cannot carry: `id`, `status`, `signature`, `forwarded_from`, `event`.

The body comes from the `text/html` part when there is one, through an HTML-to-Markdown step, and from `text/plain` otherwise.
Neither existing converter fits:

- `html_to_plain` (`crates/mp-core/src/parse.rs:445`) is `html2text` in plain mode, the renderer for received HTML; it keeps the text and drops the Markdown structure.
- `html_to_markdown` (`crates/mp-core/src/parse.rs:485`) is a regex converter for signatures, documented as "deliberately narrow": anchors, `<br>`, paragraphs, bold, italic and images.

The pull needs the subset `markdown_to_html` (`src/send.rs:156`, pulldown-cmark with strikethrough and tables) produces, plus whatever Outlook and Apple Mail add: headings, lists, block quotes, code, links, emphasis.
`html2text` 0.14, already a dependency of `mp-core`, exposes the `TextDecorator` trait (`html2text::render::TextDecorator`) and `config::with_decorator`.
A Markdown decorator in `crates/mp-core/src/parse.rs` built on them gives a Markdown-preserving reverse without a new crate.
A dedicated crate (`htmd`, a Turndown port) would convert tables and nested lists better, and adopting it is a decision for Sylvain after the usual due diligence.

Only the part above the quote boundary is replaced.
The text below `QUOTE_MARKER` and the companion `.html` stay as they are, because Outlook and Apple Mail rewrite a quoted thread in ways mp would not round-trip.
The boundary in the server HTML is mp's own quote `<div>` when the comment survived, and otherwise the first `divRplyFwdMsg` (Outlook) or `blockquote type="cite"` (Apple Mail); the probe's dumps confirm the markers.

A pull into an `approved` draft demotes it to `status: draft`: a send must never carry content the user did not approve.
A pull into a draft with an open outbox row (`active_submission_for_draft`, `src/outbox.rs:773`) or an armed undo-send hold (`src/daemon/hold.rs`) waits until that settles.

#### Conflict

When both sides changed since the last convergence, the local text is kept as `<name>.conflict.md` beside the draft, and the draft itself takes the server's version.
The conflict copy gets a fresh `id:` and `conflict_of: <original id>`.
A fresh id is required: two files with the same `id:` shadow each other in the watcher's inventory with whichever settled last (`BACKLOG.md:38`).
A conflict copy is never pushed; the user merges it and deletes it, or removes `conflict_of:` to keep it as a draft of its own, which is then pushed like any other.

#### Retiring a draft

Retiring moves the file and its HTML companion into `drafts/.retired/` and deletes nothing.
The indexer and the watcher read only the top level of `drafts/` (`WalkDir::new(dir).max_depth(1)`, `src/store/drafts.rs:362-363`; `std::fs::read_dir`, `src/daemon/watch.rs:420-427`), so a retired file leaves every list without a code change there.
`.retired/` doubles as the tombstone set: a server copy whose marker names a retired id is deleted, never imported back.
Files in `.retired/` older than 30 days are pruned by the reconcile.

Three paths retire:

- The server copy is gone: the draft was sent or deleted in another client.
- `draft.discard` (`src/daemon/methods/draft.rs:767`), which today calls `delete_indexed_draft` and deletes the file; it now moves the file and enqueues `DeleteDraft`.
- A send that reached every recipient, which today deletes the file (`settle_sent_draft`, `src/draft.rs:189`) and now moves it.

A file removed by hand from `drafts/` arrives as `WatchEvent::DraftRemoved` with no retired twin.
Its server copy may be the only one left, so the reconcile pulls it into `.retired/` before it enqueues the `DeleteDraft`.

#### Send

`send_draft` (`src/send.rs:2216`) commits the outbox row before SMTP, records the result, then settles the draft (`:2285-2295`).
The server copy's delete joins the outbox's transition out of `pending_send`, without a fifth state:

- `outbox::enqueue` (`src/outbox.rs:644`) records the draft's server identity on the row, in a new nullable `outbox.draft_copy` column holding `{mailbox, message_id, uid, uidvalidity}`.
  The rebuild carries open outbox rows across a store drop (`src/store/rebuild.rs:1-15`), and the column goes with them.
- `record_submission` (`src/outbox.rs:819`) and `record_per_recipient` (`src/outbox.rs:912`), the two transitions out of `pending_send`, insert a `DeleteDraft` `pending_ops` row in the same transaction that moves the row to `sent_pending_append` or `done`, when the row names a draft copy and every recipient was delivered.
- The condition is the one `settle_sent_draft` retires the file under (`src/draft.rs:182-187`), so a partly delivered draft keeps both its local file and its server copy.

The outbox's `done` keeps meaning "sent and filed", and the delete is a server mutation of the kind the queue already retries and converges.
A failed delete retries on the queue's backoff and parks `failed` past its budget with `Rollback::None`.
The stale server copy then shows in Outlook and Apple Mail as a draft that was in fact sent; mp's reconcile sees a marker that names a retired id and enqueues `DeleteDraft` again, so it converges at the next sync that reaches the server.
A `DeleteDraft` whose copy is already gone is a not-found, which the drain treats as success (`src/pending_ops.rs:20-28`).

#### The per-account switch

`[accounts.drafts] mirror = true | false` in `config.toml`, read by the daemon at reload.
It defaults to `true` once the probe has run, and `false` keeps today's local-only drafts for an account whose server mangles drafts.
The server folder is the account's `drafts` role from [#0141](0141-mailbox-auto-discovery.md) when that has landed, and otherwise the folder carrying the `\Drafts` SPECIAL-USE attribute, then the literal `Drafts`, which both accounts use.

### Slice 2, foreign drafts

A server draft that links to no local file and carries no marker was started in Outlook, Apple Mail or the Proton web app.
The reconcile imports it as a new file in the drafts directory:

- a fresh `id:` and the server keys, with `server_message_id` taken from the draft's own Message-ID;
- `to`, `cc`, `bcc`, `subject`, `from`, `reply_to` from its headers, and `in_reply_to:` from its `In-Reply-To`;
- `status: draft`;
- the body through the pull's converter, and the attachments as files as above.

From then on it is treated as mp-owned.
An imported draft is not pushed until it is edited locally, and a draft only read in mp keeps the HTML its client wrote.
The first local edit replaces the server copy with mp's rendering, which carries the marker and loses any formatting Markdown cannot express.
The import never writes a second file for an `id:` the watcher already holds (`DraftWatch::resolve`, `src/daemon/watch.rs:584-586`).

#### The received list

Today the server Drafts folder lists as a received mailbox when it is synced at all (the TUI's `build_mailboxes`).
Once slice 2 lands, the `drafts`-role folder is hidden from the received mailbox list in the TUI (`build_mailboxes`) and the desktop sidebar, and the Drafts view, `draft.list` over the directory, is the one place a draft shows.
It stays synced, because the reconcile reads it.
A merged view that lists server drafts beside local files was considered and dropped: after the import every server draft has a local file, so a merge would only show each draft twice.

### Slice 3, Graph

The same contract over `src/graph.rs`, for when an account uses Graph:

- push: `POST /me/messages` creates the copy in Drafts and `PATCH /me/messages/{id}` replaces its body and recipients, so a Graph draft keeps one id across edits;
- identity: the immutable id (`Prefer: IdType="ImmutableId"`) as `server_uid`, the `internetMessageId` as `server_message_id`, and the marker as `internetMessageHeaders`, which Graph accepts only on create;
- change detection: `changeKey` and `lastModifiedDateTime`;
- delete: the existing `delete_message` (`src/graph.rs:2203`);
- foreign drafts: the `isDraft` messages of `/me/mailFolders/drafts`.

No Graph account exists today, and nothing here can be verified live.

### Protocol surface

No new method and no new event kind.
Every change to a draft is a write to its file, which keeps the watcher the one publisher of `draft.changed` and `state.remove`:

- a pull, an import and the daemon's write of the `server_*` keys all reach clients as `draft.changed`;
- a retire is a file leaving the top level, which the watcher publishes as the removal of `draft:<account>/<id>`.

`DraftRow` (`crates/mp-protocol/src/state.rs:147`) and the `draft.list` row gain an optional `server` member, `{state, synced_at}`, defaulting to absent.
The change is additive: the protocol stays `{min: 1, max: 1}` and an older client ignores the member.
`state` is derived on each reparse:

- `local` for a draft with no `server_message_id` or an account whose switch is off;
- `pending` for a file whose hash differs from `server_synced_hash`;
- `synced` when it matches;
- `conflict` for a draft some `conflict_of:` names, and for the conflict copy itself.

A push parked `failed` is not visible in the file.
The drain publishes it as the existing `mutations.rolled_back` lifecycle kind (`docs/daemon-protocol.md:1091`), and `draft.list` joins the failed `pending_ops` row to answer `state: failed`.
No `state.invalidate` scope changes: drafts never travelled as invalidations, and the Drafts folder's counts stop mattering once slice 2 hides it.

### Invariants kept

- Drafts `.md` files are the truth and the `drafts` table is derived (`docs/architecture.md:11-12`); the server copy is a mirror of the file, and a server change reaches mp only by rewriting the file.
- Nothing in the store is the only copy of anything the user typed (`docs/architecture.md:10`): the server identity lives in the frontmatter, and a lost queue row is re-derived from the file.
- Blob files are written before the transaction that references them: `PushDraft` carries no blob, and the attachments a pull writes are plain files written before the frontmatter names them.
- The per-account engine lock (`src/engine_lock.rs`) covers the drain and the reconcile; the watcher stays lock-free.
- Exactly-once submission (`submission_started_at`, `docs/architecture.md:293`) is untouched: the server copy's delete is queued after the SMTP verdict and never gates it.
- No two files share an `id:` (`BACKLOG.md:38`): conflict copies and imports mint fresh ids, and the marker import writes only an id the directory does not hold.

### Decisions for Sylvain

- The frontmatter carries the volatile `server_uid`, so every push rewrites the file the editor may have open.
  Neovim with `autoread` reloads an unmodified buffer silently; a buffer with unsaved edits gets the "file changed" prompt after each push.
  The alternative keeps only `server_message_id` in the frontmatter and the UID and hash in the `drafts` table, at the cost of a store rebuild that loses which local edits are unpushed.
- The HTML-to-Markdown step: a `TextDecorator` on the existing `html2text`, or the `htmd` crate.
- `approved` does not travel to the server or to the other machine: approval stays a local act on each machine.
- `.retired/` is pruned after 30 days.

## Rollout steps

1. Probe: `examples/draft_probe.rs`, run against `tum` and `proton` from the Mac with Outlook and Apple Mail, and "Probe results" filled in.
   The linking rule and the Proton switch default are settled here, before any production code.
2. Contract: the six `server_*` keys and `conflict_of` in `EmailFrontmatter`, the `server` member of `DraftRow` and its fixture, `docs/daemon-protocol.md`.
3. Push: `build_server_draft` and the shared MIME assembly, `ServerOp::PushDraft` and `DeleteDraft`, the drain's APPEND, UID read-back and UID delete, the watcher hand-off in `src/daemon/watch.rs`, the switch in `config.toml`.
4. Send and discard: `outbox.draft_copy`, the `DeleteDraft` in both transitions out of `pending_send`, `draft.discard` and `settle_sent_draft` moving into `.retired/`.
5. Reconcile and pull: `messages.draft_marker`, the eviction exemption, the reconcile pass, the HTML-to-Markdown step, conflict copies, the marker import from the other machine.
6. Foreign drafts: the import, and the received list hiding the `drafts`-role folder in the TUI and the desktop.
7. Graph, once an account uses it.

Each step lands on its own and leaves the tree green; until step 3 nothing reaches the server.

## Acceptance criteria

- [ ] The probe has run on `tum` and `proton` with Outlook and Apple Mail, and "Probe results" records every item of the list above.
- [ ] A draft created with `mp new` on the Mac appears in Outlook's and Apple Mail's Drafts within one quick tick, with its recipients, Bcc included, its subject, its body and its attachments.
- [ ] An edit saved in Neovim replaces the server copy, and the Drafts folder holds one copy of the draft afterwards.
- [ ] An edit made in Outlook reaches the local file at the next quick tick, with the quoted section below `QUOTE_MARKER` unchanged.
- [ ] The same draft, created on the Mac, appears in the home server's `drafts/` and converges back after an edit there.
- [ ] Editing a draft both in Neovim and in Outlook between two syncs leaves the server's version in the draft and the local text in `<name>.conflict.md` with a fresh `id:`.
- [ ] Sending a draft with `mp send` removes its server copy; a failed delete retries and the copy is gone after the next sync that reaches the server.
- [ ] Sending or deleting a draft in Outlook moves the local file to `drafts/.retired/`, and it is not imported back.
- [ ] A draft started in Outlook appears in mp's Drafts view as a Markdown file, and the server Drafts folder no longer lists as a received mailbox.
- [ ] A pull into an approved draft leaves it `status: draft`.
- [ ] Killing the daemon between APPEND and delete leaves one copy after the next drain.
- [ ] `cargo test --workspace` passes, with tests over a fake IMAP Drafts folder for each reconcile case above.

## Cross-references

- [data-access-layer.md](../plans/data-access-layer.md), decision 2, amended for this ticket.
- [#0141](0141-mailbox-auto-discovery.md): the `drafts` role and the `Drafts` key collision.
- [#0050](0050-selector-contract-drafts-index.md): draft identity by `id:` and the drafts index.
- [#0060](0060-retention-enforcement.md) and [#0085](0085-on-open-body-refetch.md): the eviction the Drafts folder is exempted from.
- [#0081](0081-qresync-uidplus.md): `UIDPLUS`, which the UID delete uses.
- [#0122](0122-daemon-ownership-moves.md): the draft watcher and the duplicate-id follow-up.
- [#0133](0133-debounced-drain-after-a-queued-mutation.md): the debounced drainer the push rides on.
- `.agents/research/2026-07-12-architecture-rethink-decision-doc.md`, Option C, which first proposed syncing drafts through `pending_ops`.
- Docs that change when this ships:
  - `docs/architecture.md`: the invariant at `:11-12`, the `drafts` and `pending_ops` store bullets at `:227-228`, the Send section at `:279-307` and the retirement at `:299`;
  - `docs/daemon-protocol.md`: the `draft.*` family at `:661`, the draft row at `:212`, `draft.changed` at `:1060`, and the protocol changelog at `:1229`;
  - `docs/daemon-operations.md`: the draft watcher at `:310`;
  - `docs/parity-matrix.md`: LST-01 at `:262`, DFT-02 at `:609`, DFT-12 at `:712`, and a new DFT row for the mirror;
  - `website/src/pages/draft-format.astro` (the frontmatter keys and "After Sending", `:145-160`), `website/src/pages/commands.astro:282` ("Drafts are the only local-only data") and `website/src/pages/config.astro` for the switch;
  - `CHANGELOG.md` and `BACKLOG.md`.

## Out of scope

- `In-Reply-To` and `References` on the sent message: `build_draft_message` writes neither today, so mp's sent replies do not thread by header in other clients; PERSO-137 adds them, as a prerequisite of #0142.
- Two clients sending the same draft at once, mp and Outlook: neither can see the other's submission, and the server copy's delete does not prevent it.
- Mirroring signatures, approval state or the `event:` block of an invite draft.
- A three-way merge of conflicting edits; the conflict copy is the whole answer.
- Drafts of an account with no server Drafts folder.
