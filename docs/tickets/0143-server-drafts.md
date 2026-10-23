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
  Output goes to stdout and to `/var/tmp/mp-draft-probe-<account>-<id>.txt`; it carries the probe's drafts plus the headers and first 2 KiB of any draft saved in the folder between the APPEND and the dump, which is what step 6 is for.

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
The rule is a chain tried in order, and the probe decided which links stay in the chain ("Probe results", "Decisions"): Message-ID, then the fallback, on both servers; neither the UID nor the marker header survives a foreign edit anywhere.
The four candidate links, for reference:

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
Bridge keeps the Message-ID and the attachment, so the link holds on Proton and its switch defaults to on.

### Probe results

Sylvain ran the probe on 2026-10-20 against both accounts.
The dumps are in `.sync/draft-probe/`, one `.txt` per draft holding every dump of it, with the appended bytes (`.eml`) and the append state (`.json`) beside it.
On Proton the edits, the send and the delete came from the Proton Mail app, since neither Outlook nor Apple Mail holds the Proton account.

#### Both servers

- No foreign edit keeps the UID: Outlook, Apple Mail and the Proton Mail app each save a new message.
- Outlook and the Proton Mail app keep the Message-ID; Apple Mail gives the edited draft a new one.
- `X-Mailypoppins-Draft-Id` never survives: Outlook strips it, and Bridge drops it already at mp's APPEND.
- The `mp:quote`, `mp:sig-start` and `mp:sig-end` comments disappear on every foreign edit; they survive mp's own APPEND on both servers.
- Bcc survives every edit.
- The attachment survives every edit.
- Every copy carries `\Draft` and `\Seen`.
- Both servers offer `UIDPLUS`, `MOVE` and `IDLE`, and neither offers `CONDSTORE`, `QRESYNC` or `SPECIAL-USE`.
  Both still mark the role folders in their plain LIST: TUM `Drafts` `\Drafts`, `Sent Items` `\Sent`, `Deleted Items` `\Trash`, `Junk Email` `\Junk`; Proton `Drafts` `\Drafts`, `Sent` `\Sent`, `Trash` `\Trash`, `Spam` `\Junk`, `Archive` `\Archive`, `All Mail` `\All`, `Starred` `\Flagged`.
  TUM's `Archive` carries no archive mark.

#### tum (TUM Exchange, Outlook and Apple Mail)

Dumps: `.sync/draft-probe/mp-draft-probe-tum-c09dc7f7d4f0d441.txt` (steps 1 to 3), `mp-draft-probe-tum-af4b6b4398f1bb5b.txt` (step 4), `mp-draft-probe-tum-a0a14d3abeebf223.txt` (steps 5 and 6).

- mp's APPEND is stored as sent: `multipart/mixed` around `multipart/alternative` (`text/plain`, `text/html`) plus the PDF, the marker and every HTML comment intact.
  The probe found no message to reply to on this account, so the TUM drafts carried no `In-Reply-To`, and its survival on Exchange is untested.
- Outlook edit: new UID (18564 to 18566), Message-ID kept, marker gone, the body reduced to `text/html` plus the PDF, the comments gone, Bcc kept with Outlook's display name.
- Apple Mail edit of that copy: new UID (18567), new Message-ID (`<0DC386F6-…@tum.de>`), the body `multipart/alternative` with a 2-byte `text/plain` and a `multipart/related` holding the HTML and the PDF as an inline part.
- Sent from Outlook: the Drafts copy is gone within a minute and is not in Deleted Items.
- Deleted in Apple Mail: the copy moves to `Deleted Items` with its Message-ID, marker, flags and MIME unchanged.
- Foreign drafts: Apple Mail writes `multipart/alternative` with a near-empty `text/plain`; Outlook writes a single `text/html` part in `iso-8859-1`.

#### proton (Proton Bridge, Proton Mail app)

Dumps: `.sync/draft-probe/mp-draft-probe-proton-a66696b5145d0449.txt` (steps 1, 2 and the send of step 4), `mp-draft-probe-proton-b9399c52face5f2d.txt` (untouched snapshots, then a send), `mp-draft-probe-proton-df227efbf1256ee8.txt` (steps 5 and 6).

- Bridge rewrites mp's APPEND: the marker and `In-Reply-To` are dropped, `References` becomes a single `@protonmail.internalid` id, and the body keeps only the `text/html` part beside the PDF; the Message-ID, Bcc and the HTML comments survive.
- Proton Mail app edit: new UID (26 to 27), Message-ID kept, the comments gone, the PDF and Bcc kept.
- Sent from the Proton Mail app: the Drafts copy is gone; for one of the two sent drafts a copy with the same Message-ID then sat in `Trash`.
- Deleted in the Proton Mail app: the copy is gone from Drafts and is not in `Trash`.
- A foreign draft from the Proton Mail app is a single `text/html` part with a Proton internal Message-ID, and it reached Bridge's Drafts only on the recheck a minute later.

#### Leftovers to delete

The probe's test drafts the last dumps show, to be re-listed from the server before anything is deleted:

- tum `Drafts`: UID 18567 (the Apple Mail edit of probe `c09dc7f7d4f0d441`), UID 18576 (Apple Mail's foreign draft), UID 18577 (Outlook's foreign draft).
- tum `Deleted Items`: UID 2394 (probe `a0a14d3abeebf223`, deleted in Apple Mail).
- proton `Drafts`: UID 30 (the Proton Mail app's foreign draft).
- proton `Trash`: UID 1530 (probe `b9399c52face5f2d`).

### Slice 1, mp-owned drafts two-way

#### The frontmatter keys

`EmailFrontmatter` (`crates/mp-core/src/types.rs:310`) carries today `id`, `to`, `cc`, `bcc`, `subject`, `status`, `from`, `reply_to`, `attachments`, `sent_at`, `sent_via`, `message_id`, `in_reply_to`, `forwarded_from`, `date`, `signature` and `event`; `parse_email_draft` (`crates/mp-core/src/draft.rs:1700`) deserialises it whole.
`message_id` is the Message-ID of the sent message, written by `mark_draft_sent` (`crates/mp-core/src/draft.rs:1899`), and is not reused here.

Slice 1 adds two optional keys, each `skip_serializing_if = "Option::is_none"` so a draft without them parses unchanged:

- `server_message_id`: the Message-ID every server copy of this draft carries, minted once by mp when the draft is first pushed and kept for its life.
  It differs from the Message-ID the send mints (`src/send.rs:2829-2832`), so the sent message and the draft are never confused by a Message-ID search.
- `conflict_of: <id>`: marks a conflict copy (below).

The daemon writes `server_message_id` with `rewrite_frontmatter_scalars` (`crates/mp-core/src/draft.rs:1540`), the surgical writer `mark_draft_sent` uses, which leaves every other byte of the file as it was.
That write happens once in the draft's life, at its first push.
No later push touches the file; the server state a push changes lives in the store.

#### The `draft_sync` table

Every push replaces the server copy and changes its UID.
The push runs a few seconds after the save that triggers it (the watcher's poll and settle, then `DRAIN_DEBOUNCE`).
A UID kept in the file would therefore be rewritten during every editing session, and Neovim would prompt "file changed" whenever the user had typed between the save and the push.

The volatile state lives in a store table, `draft_sync`, keyed by `(account, draft_id)`:

- `mailbox`: the server folder the copy lives in.
- `uid` and `uidvalidity`: the UID of the current copy and the folder's UIDVALIDITY when it was recorded, because a UID means nothing without it.
- `synced_at`: Unix seconds of the last push or pull that converged.
- `synced_hash`: a hash of the file's bytes at that convergence.
  A file whose hash differs has changed locally since; a file whose hash matches has not.

`draft_sync` is created with `CREATE TABLE IF NOT EXISTS` at open, beside `ensure_additive_indexes` and `ensure_additive_columns` (`src/store/schema.rs:404`, `:444`), with no `SCHEMA_VERSION` bump.
It is separate from the derived `drafts` table, which `refresh_reporting` (`src/store/drafts.rs:149`) deletes and rewrites on every refresh.
Retiring a draft deletes its row.

The table holds only sync bookkeeping, which the server copy and the file determine between them.
A store rebuild drops it with the rest of the file, since only open `outbox` rows are carried (`src/store/rebuild.rs:1-15`), and the reconcile re-derives each row:

1. It finds the server copy by `server_message_id`, with `UID SEARCH HEADER Message-ID` in the Drafts folder.
2. It renders the copy as a pull would and compares the result with the file.
3. Equal means synced: the row is written from the copy's UID and the file's hash.
4. Different means the side that changed is unknown: the draft takes the server's version and the local text goes to `<name>.conflict.md` with a fresh `id:`, the path a concurrent edit takes ("Conflict" below).
5. No copy found means the draft is pushed again.
   A draft sent or deleted elsewhere while the table was missing comes back as a duplicate, which costs a deletion and loses nothing the user typed.

The same recovery covers a crash between an APPEND and the write of its row.

#### When a push is owed

- The draft watcher (`src/daemon/watch.rs`, the one-second poll with a 300 ms settle, `:52-55`) settles a changed file into `WatchEvent::DraftChanged`.
  `src/daemon/runtime/watcher.rs` is the IDLE watcher for received mail and takes no part in this.
- The watcher opens no store and takes no engine lock (`src/daemon/watch.rs:606-610`), and that stays true.
  It reads the synced hashes from an in-memory copy of each account's `draft_sync` rows, which the account's runtime loads when it comes up and updates on every write of the table.
  Its loop, `spawn` (`src/daemon/watch.rs:616`), gains the `RuntimeTable` and hands each settled change of a draft whose hash differs from its row's `synced_hash`, or that has no row, to that account's runtime, which enqueues the push and calls `request_drain` (`src/daemon/runtime/account.rs:463`).
  An account whose runtime has not loaded its rows yet hands nothing over; the next reconcile finds what is owed.
- The `draft.*` commands that write a file (`draft.create`, `draft.reply`, `draft.forward`, `draft.create_from_message`, `draft.attach`, `draft.detach`, `draft.approve`, `draft.demote`) need no hook of their own: the watcher notices their writes like any other, as it already does for `draft.approve` (`docs/daemon-protocol.md:748`).
- A draft with `conflict_of:` set is never pushed.
- An account whose switch is off pushes nothing (see "The per-account switch").

#### The push as a queued op

The push is a `pending_ops` row (`src/pending_ops.rs`), drained by the account's drainer after `DRAIN_DEBOUNCE` (`src/daemon/runtime/drainer.rs:76`, 1.5 s) like a queued archive.
`ServerOp` (`src/ops.rs:81`) gains two kinds:

- `PushDraft { draft_id, hash }`: render the file as it is at drain time, APPEND it, retire the previous copy.
  `hash` is the hash of the bytes the push is owed for, kept current while the row waits, so a push parked `failed` is not queued again for the same bytes until a fifteen-minute cool-down has passed, while the next edit queues one at once.
  A retry that fails again replaces the draft's old failed row, and one for the same bytes is not announced a second time.
- `DeleteDraft { mailbox, message_id, uid, uidvalidity }`: remove every copy of one draft from the server folder.

`PushDraft` names the draft and carries no bytes.
The drain reads the file when it runs, which makes a burst of edits one APPEND of the last state.
The enqueue is a no-op when a queued `PushDraft` for the same id already exists.
It also keeps the blob invariant trivially: there is no blob to write before the transaction.
`target_message_id` is NULL, because the op addresses a file and no `messages` row.

The drain of a `PushDraft`:

1. Parse the file.
   A file that is gone or does not parse retires the queue row with nothing done, and so does a file whose hash equals its `draft_sync` row's `synced_hash`, which a pull or a burst of saves leaves behind.
   A file with a `server_message_id` and no `draft_sync` row runs the recovery of "The `draft_sync` table" instead of a push.
2. A first push mints `server_message_id`, writes it into the file, reads the file again and resolves the folder.
3. Build the server copy (below).
4. APPEND it to the folder with `(\Draft \Seen)`.
5. Recover the new UID with `UID SEARCH HEADER MESSAGE-ID`, as `ImapSentMailbox::append` does (`src/imap_client/sent.rs:108-118`), because `async-imap` 0.11 drops `APPENDUID` (`src/imap_client/sent.rs:9-20`).
   Every copy shares the Message-ID, so the new copy is the highest UID the search returns.
6. Delete the previous copy by the UID its `draft_sync` row names: `UID STORE +FLAGS (\Deleted)` and `UID EXPUNGE` when the server offers `UIDPLUS`.
   Without `UIDPLUS` the old copy is only flagged `\Deleted`, since a plain `EXPUNGE` would also expunge whatever another client flagged in that folder; the reconcile ignores flagged copies.
   The existing `delete_email_on_server` (`src/imap_client/ops.rs:57`) finds its target by Message-ID, which here matches every copy, so it is not reused for this step.
7. Upsert the draft's `draft_sync` row with the folder, the new UID, UIDVALIDITY, the time, and the hash of the bytes the drain read, then have the draft's row republished ("Protocol surface").
   The file stays untouched, so an editor holding it open sees no change.

APPEND comes before the delete, so a crash between them leaves two copies rather than none, and the reconcile removes the older one.
A refusal follows the queue's backoff (`crate::outbox::backoff_secs`) and parks the row `failed` past its budget with `Rollback::None` (`src/pending_ops.rs:97`): the local file is the truth, so there is nothing local to undo.

`pending_ops` is a cache table that a store rebuild drops (`docs/architecture.md:228`), and so is `draft_sync`.
A lost `PushDraft` is re-derived at the next reconcile, because the file's hash still differs from its row's `synced_hash`, and a lost row is re-derived as "The `draft_sync` table" describes.
`server_message_id` is the one piece of sync state the reconcile could not re-derive from the server, so it lives in the frontmatter, which a rebuild leaves alone.

#### Building the server copy

`build_draft_message` (`src/send.rs:2757`) cannot be reused as it stands: it refuses a draft that is not approved (`:2764-2770`), refuses one without recipients (`:2814-2816`), mints a fresh Message-ID per build (`:2829-2832`) and lets lettre drop the Bcc header.
Slice 1 splits its MIME assembly (`:2866-2947`: the HTML from `markdown_to_html`, the companion `.html` quote, the alternative part, the attachments) into a shared function, and adds `build_server_draft` beside it, which:

- accepts any status and an empty recipient list or subject, since a draft is allowed to be unfinished;
- stamps `server_message_id` and `X-Mailypoppins-Draft-Id: <id>`;
- keeps Bcc with lettre's `keep_bcc()`, because a draft's Bcc is part of what the user wrote;
- writes `In-Reply-To` and `References` from `in_reply_to:`, which Outlook and Apple Mail need to show a reply draft as a reply;
- keeps the signature sentinels and `QUOTE_MARKER` as HTML comments in the HTML part, for a pull of an unedited copy; the probe shows them surviving mp's own APPEND on both servers and disappearing on every foreign edit, where the pull falls back to the quote wrappers ("Pull").

The send path writes the same two headers since PERSO-137 (`build_draft_message`).

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

- No `draft_sync` row: the recovery of "The `draft_sync` table".
- The linked server copy is unchanged (the row's UID, or the rule's equivalent) and the file's hash matches the row: nothing to do.
- The file's hash differs and the server copy is unchanged: a push is owed; enqueue `PushDraft` if none is queued.
- The server copy changed and the file's hash matches: pull.
- Both changed since the row's `synced_at`: conflict.
- No server copy links to it: the copy was sent or deleted elsewhere; retire the file.
- Two or more copies link to it, which a crash between APPEND and delete or a concurrent push from the other machine leaves behind: the newest by UID is the copy; an older copy whose body equals a version mp pushed is deleted, and any other older copy is pulled as a conflict copy.

For each server copy carrying a marker that names no local file and no file in `drafts/.retired/`: it is a draft the other machine's mp pushed, and it is written as a new local file under that `id:` with the copy's `server_message_id`, beside a `draft_sync` row for the copy.
Without this rule the Mac and the home server would not converge in slice 1, since each would only ever see its own drafts.

The IDLE watcher hears only INBOX (`src/daemon/runtime/watcher.rs:21-28`), so a tick alone left the Drafts folder unread for up to `imap.sync_interval_secs`, fifteen minutes by default.
As built, a drafts poll (`src/daemon/runtime/drafts_poll.rs`) runs the reconcile on its own once a minute between ticks, which is close to what Outlook and Apple Mail show between each other.

#### Pull

A pull rewrites the body and the headers of the local file from the server copy and keeps every frontmatter key the server cannot carry: `id`, `status`, `signature`, `forwarded_from`, `event`, `server_message_id`.
A copy that carries mp's source whole is that file, and its `status` comes with it ("Decisions", approval).
It writes the file first and then the draft's `draft_sync` row, with the copy's UID and the hash of the bytes it wrote, so the watcher's settle of that write owes no push.

The body comes from the `text/html` part when there is one, through an HTML-to-Markdown step, and from `text/plain` otherwise.
Neither existing converter fits:

- `html_to_plain` (`crates/mp-core/src/parse.rs:445`) is `html2text` in plain mode, the renderer for received HTML; it keeps the text and drops the Markdown structure.
- `html_to_markdown` (`crates/mp-core/src/parse.rs:485`) is a regex converter for signatures, documented as "deliberately narrow": anchors, `<br>`, paragraphs, bold, italic and images.

The pull needs the subset `markdown_to_html` (`src/send.rs:156`, pulldown-cmark with strikethrough and tables) produces, plus whatever Outlook and Apple Mail add: headings, lists, block quotes, code, links, emphasis.

The step is the `htmd` crate (0.5.5, Apache-2.0, a port of turndown.js), chosen on 2026-10-14 over a `TextDecorator` on `html2text`.
`html2text`'s decorators produce wrapped plain text with footnote links, no escaping and ASCII-box tables, so valid Markdown would need a hand-written emitter on top of them.
`htmd` emits CommonMark with GFM tables, escaping and nested block quotes, and takes per-tag handlers (`add_handler`) and `skip_tags`.
It pulls `html5ever` 0.39 and `markup5ever_rcdom` 0.39, while `html2text` 0.14.4 in `Cargo.lock` uses `html5ever` 0.31, so `html5ever` compiles twice; that cost is accepted.
`html2text` keeps rendering received HTML as plain text.

The converter lives in `crates/mp-core/src/parse.rs` beside `html_to_plain`, configured with:

- `skip_tags` for `script`, `style` and `head`, without which the text of a `<style>` block leaks into the body;
- a handler for Word's fake lists, the `mso-list` paragraphs whose number sits inside `<![if !supportLists]>`, turning them into Markdown lists;
- handlers for the quote wrappers, Apple Mail's `blockquote type="cite"`, Outlook's `div#divRplyFwdMsg` and `div#appendonsend`, Outlook mobile's `div#mail-editor-reference-message-container`, and the top-bordered block Outlook opens a quoted `From:`/`Sent:` header with, which map back to the `QUOTE_MARKER` section;
- a trim of trailing `&nbsp;`.

Only the part above the quote boundary is replaced.
The text below `QUOTE_MARKER` and the companion `.html` stay as they are, because Outlook and Apple Mail rewrite a quoted thread in ways mp would not round-trip.
The boundary in the server HTML is mp's own quote `<div>` when the comment survived, and otherwise the first of the quote wrappers above; the probe's dumps confirm the selectors.

A pull into an `approved` draft demotes it to `status: draft`, unless the carried source reproduces a file mp approved and pushed on the other machine, and every attachment is here or carried by the copy: a send must never carry content the user did not approve.
A pull into a draft with an open outbox row (`active_submission_for_draft`, `src/outbox.rs:773`) or an armed undo-send hold (`src/daemon/hold.rs`) waits until that settles.

#### Conflict

When both sides changed since the last convergence, the local text is kept as `<name>.conflict.md` beside the draft, and the draft itself takes the server's version and its `draft_sync` row, as after a pull.
The conflict copy gets a fresh `id:` and `conflict_of: <original id>`.
A fresh id is required: two files with the same `id:` shadow each other in the watcher's inventory with whichever settled last (`BACKLOG.md:38`).
A conflict copy is never pushed; the user merges it and deletes it, or removes `conflict_of:` to keep it as a draft of its own, which is then pushed like any other.

#### Retiring a draft

Retiring moves the file and its HTML companion into `drafts/.retired/` and deletes nothing.
The indexer and the watcher read only the top level of `drafts/` (`WalkDir::new(dir).max_depth(1)`, `src/store/drafts.rs:362-363`; `std::fs::read_dir`, `src/daemon/watch.rs:420-427`), so a retired file leaves every list without a code change there.
A draft sent, discarded or removed by hand is a tombstone in `.retired/`: a server copy whose `Message-ID` or marker names it is deleted, never imported back.
A draft retired because its copy went is kept as `<name>.copy-gone.md` and is no tombstone: a copy under its `Message-ID` that comes back (the other machine pushed it again) is pulled back into `drafts/` rather than deleted.
Otherwise one machine's mistaken retire would leave a tombstone that deletes the other machine's next push, and the draft would be retired on both.
Files in `.retired/` older than 30 days are pruned by the reconcile.

Three paths retire:

- The server copy is gone: the draft was sent or deleted in another client.
  Two reconciles in a row under one UIDVALIDITY must miss the copy (the `draft_absence` table), so one short listing retires nothing.
- `draft.discard` (`src/daemon/methods/draft.rs:767`), which today calls `delete_indexed_draft` and deletes the file; it now moves the file and enqueues `DeleteDraft`.
- A send that reached every recipient, which today deletes the file (`settle_sent_draft`, `src/draft.rs:189`) and now moves it.

A file removed by hand from `drafts/` arrives as `WatchEvent::DraftRemoved` with no retired twin.
Its server copy may be the only one left, so the reconcile pulls it into `.retired/` before it deletes the copy.
As built, the reconcile acts on a row whose file is missing from two reconciles in a row, and a file whose frontmatter does not parse still counts as present when a line scan reads its `id:`, so a draft whose YAML is half typed is never taken for one removed by hand.

#### Send

`send_draft` (`src/send.rs:2216`) commits the outbox row before SMTP, records the result, then settles the draft (`:2285-2295`).
The server copy's delete joins the outbox's transition out of `pending_send`, without a fifth state:

- `outbox::enqueue` (`src/outbox.rs:644`) records the draft's server identity on the row, in a new nullable `outbox.draft_copy` column holding `{mailbox, message_id, uid, uidvalidity}`, read from the draft's frontmatter and its `draft_sync` row.
  The rebuild carries open outbox rows across a store drop (`src/store/rebuild.rs:1-15`), and the column goes with them.
- `record_submission` (`src/outbox.rs:819`) and `record_per_recipient` (`src/outbox.rs:912`), the two transitions out of `pending_send`, insert a `DeleteDraft` `pending_ops` row in the same transaction that moves the row to `sent_pending_append` or `done`, when the row names a draft copy and every recipient was delivered.
- The condition is the one `settle_sent_draft` retires the file under (`src/draft.rs:182-187`), so a partly delivered draft keeps both its local file and its server copy.

The outbox's `done` keeps meaning "sent and filed", and the delete is a server mutation of the kind the queue already retries and converges.
The send method asks the account's runtime for a drain once the send ends, so the delete goes out seconds after the send; until 2026-10-23 it waited for the next sync tick, and a TUM draft sent from mp stayed in Outlook's Drafts for eight minutes.
A failed delete retries on the queue's backoff and parks `failed` past its budget with `Rollback::None`.
The stale server copy then shows in Outlook and Apple Mail as a draft that was in fact sent; mp's reconcile sees a marker that names a retired id and enqueues `DeleteDraft` again, so it converges at the next sync that reaches the server.
A `DeleteDraft` whose copy is already gone is a not-found, which the drain treats as success (`src/pending_ops.rs:20-28`).

Since approval travels, mp on the Mac and mp on the home server can both hold the same approved draft, and a send on each would send it twice.
A send of a draft with a `server_message_id` on an account that mirrors therefore claims the server copy before it submits (`draft_mirror::send_guard`, run by `send.draft` and `send.approved` alike, after any undo-send hold has run out):

- Under the account runtime's turn, held until the submission ends, it searches the Drafts folder for the copy by `Message-ID`.
- No copy: the draft was sent or discarded on another machine or in another client; the send is refused with an error that says so, and the file is left for the reconcile to retire.
- A push never stands in for a copy that left the folder with nothing in its place (`push::push`): a row naming a copy under the current UIDVALIDITY and no copy carrying the draft's `Message-ID` is a draft sent or discarded elsewhere, and the push does nothing.
  The send key of the TUI and the desktop app approves before it sends, and the approval is a local edit that owes a push.
  On 2026-10-23 the home server's refused send pushed back the TUM draft the Mac had sent minutes before, as UID 18608; had that push drained before the send took the runtime's turn, the claim would have taken the fresh copy and sent the draft a second time.
  A local edit of such a draft is not pushed either: the reconcile retires the file, edit included, to `drafts/.retired/<name>.copy-gone.md` once two reconciles miss the copy, and a file moved back from there has no row and is pushed as a new copy, which is the deliberate way to keep it.
  Pushing the edit as a new copy was rejected, since nothing tells an edit from the approval a send makes, and it would bring back every draft sent elsewhere.
- A copy: it is removed with `UID STORE \Deleted` and `UID EXPUNGE`, and the send goes ahead only when this session saw the untagged `EXPUNGE` for every copy; a copy another session removed first refuses the send.
- After the claim the draft's `draft_sync` row is forgotten, so a send that then fails leaves a draft the owed-push scan pushes again, and the reconcile does not retire an unsent file as one whose copy went.
- A Drafts folder that cannot be reached, or a server without `UIDPLUS`, refuses the send with an error that says so: nothing then shows that the other machine has not sent the draft, so the send does not fall through.
  The cost is that a mirrored draft cannot be sent while its IMAP server is unreachable even when SMTP is; `[accounts.drafts] mirror = false` turns the guard off with the mirror.
- The outbox's `DeleteDraft` stays queued after the send and finds nothing, a not-found the drain counts as converged.
- Not closed: two sends whose reads of the copy both come before either one's `UID EXPUNGE`, a window of one round trip, both see the expunge and both go ahead; plain IMAP offers no test-and-set without `CONDSTORE`, which neither server has.

#### The per-account switch

`[accounts.drafts] mirror = true | false` in `config.toml`, read by the daemon at reload.
It defaults to `true` on every account, Proton included, and `false` keeps today's local-only drafts for an account whose server mangles drafts.
The server folder is the account's `drafts` role from [#0141](0141-mailbox-auto-discovery.md) when that has landed, and otherwise the folder carrying the `\Drafts` SPECIAL-USE attribute, then the literal `Drafts`, which both accounts use.

### Slice 2, foreign drafts

A server draft that links to no local file and carries no marker was started in Outlook, Apple Mail or the Proton web app.
The reconcile imports it as a new file in the drafts directory:

- a fresh `id:`, and `server_message_id` taken from the draft's own Message-ID;
- a `draft_sync` row for the copy;
- `to`, `cc`, `bcc`, `subject`, `from`, `reply_to` from its headers, a missing `To` as an empty `to:`, since Outlook mobile saves a forward before a recipient is chosen;
- `in_reply_to:` from its `In-Reply-To`, or `forwarded_from:` when the subject opens with `FW:`, `Fwd:` or `WG:` in any case: Outlook names a forward's source in `In-Reply-To` too, and the key decides whether the send flags the source `\Answered` and threads into its conversation, or flags it `$Forwarded` and starts a conversation of its own;
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
- identity: the immutable id (`Prefer: IdType="ImmutableId"`) in the draft's `draft_sync` row, in a nullable column slice 3 adds where IMAP uses `uid` and `uidvalidity`, the `internetMessageId` as `server_message_id`, and the marker as `internetMessageHeaders`, which Graph accepts only on create;
- change detection: `changeKey`, kept in the same row, and `lastModifiedDateTime`;
- delete: the existing `delete_message` (`src/graph.rs:2203`);
- foreign drafts: the `isDraft` messages of `/me/mailFolders/drafts`.

No Graph account exists today, and nothing here can be verified live.

### Protocol surface

No new method and no new event kind.
The watcher stays the one publisher of `draft.changed` and `state.remove`:

- a pull, an import and the first push's write of `server_message_id` are writes to the file, which reach clients as `draft.changed`;
- a write of a `draft_sync` row touches no file, so the runtime that wrote it updates the in-memory copy of the table and asks the watcher to recommit that draft, which publishes `draft.changed` with the new `server` member;
- a retire is a file leaving the top level, which the watcher publishes as the removal of `draft:<account>/<id>`.

When an account's runtime loads its `draft_sync` rows, the watcher recommits every draft of that account; until then those drafts carry no `server` member.

`DraftRow` (`crates/mp-protocol/src/state.rs:147`) and the `draft.list` row gain an optional `server` member, `{state, synced_at}`, defaulting to absent.
The change is additive: the protocol stays `{min: 1, max: 1}` and an older client ignores the member.
`state` is derived each time the watcher commits a draft, from the file and the in-memory copy of its `draft_sync` row:

- `local` for a draft with no `server_message_id` or an account whose switch is off;
- `pending` for a file whose hash differs from its row's `synced_hash`, and for one with a `server_message_id` and no row;
- `synced` when the hash matches;
- `conflict` for a draft some `conflict_of:` names, and for the conflict copy itself.

A push parked `failed` is not visible in the file.
The drain publishes it as the existing `mutations.rolled_back` lifecycle kind (`docs/daemon-protocol.md:1091`), and `draft.list` joins the failed `pending_ops` row to answer `state: failed`.
No `state.invalidate` scope changes: drafts never travelled as invalidations, and the Drafts folder's counts stop mattering once slice 2 hides it.

### Invariants kept

- Drafts `.md` files are the truth and the `drafts` table is derived (`docs/architecture.md:11-12`); the server copy is a mirror of the file, and a server change reaches mp only by rewriting the file.
- Nothing in the store is the only copy of anything the user typed (`docs/architecture.md:10`): `draft_sync` holds only sync bookkeeping, which the reconcile re-derives from the server copy and the file, `server_message_id` lives in the frontmatter, and a lost queue row is re-derived from the file.
- A push leaves the file alone once its `server_message_id` is written, so an editor holding the file open is not interrupted by the mirror.
- Blob files are written before the transaction that references them: `PushDraft` carries no blob, and the attachments a pull writes are plain files written before the frontmatter names them.
- The per-account engine lock (`src/engine_lock.rs`) covers the drain and the reconcile; the watcher stays lock-free.
- Exactly-once submission (`submission_started_at`, `docs/architecture.md:293`) is untouched: the server copy's delete is queued after the SMTP verdict and never gates it; what gates a send of a mirrored draft is the claim of its copy before the outbox row exists ("Send").
- No two files share an `id:` (`BACKLOG.md:38`): conflict copies and imports mint fresh ids, and the marker import writes only an id the directory does not hold.

### Decisions

- The frontmatter carries only `server_message_id` and `conflict_of`, and the state a push changes lives in the `draft_sync` table, re-derived by the reconcile when a rebuild drops it ("The `draft_sync` table").
- The pull converts HTML to Markdown with `htmd`, and `html2text` stays the plain-text renderer ("Pull").
- The link is the Message-ID, and when that changed, a single server draft with the same recipients, subject and reply target; a draft Apple Mail edited that matches nothing comes back as a second copy, and the old file moves to `drafts/.retired/`.
- The mirror is on by default for Proton as for TUM.
- `.retired/` is pruned after 30 days.
- An agent may delete the probe's leftover drafts from the TUM and Proton Drafts and Deleted Items folders once they are listed under "Leftovers to delete".
- No part of a server copy carries anything that is not already in the message a client would send (review finding B4, 2026-10-21).
  A copy can be sent unedited from Outlook, Apple Mail or the Proton app, and whatever its body holds then reaches every recipient, so the first build's `<!-- mp:source v1 -->` comment, the whole draft file with its `bcc:` and private frontmatter in the HTML part, is gone.
  The HTML part now ends with a mark holding two SHA-256 hashes, `<!-- mp:copy v2 file=<hash> src=<hash> -->`, which is all that tells an unedited copy from one edited elsewhere.
  The draft file rides in `X-Mailypoppins-Draft-Source` headers instead, base64 in numbered chunks of 8 KiB, at most 64 KiB in all, with its `bcc:` emptied; the Bcc comes back from the copy's own `Bcc` header, which every client treats as blind.
  Evidence for the headers: Exchange Online caps one header at 32 KB and a header block at 256 KB (Microsoft Learn, "Exchange Online limits"), and Exchange Server has the same block limit as a receive connector setting; Dovecot allows 10 MB for one header block (Dovecot CE, "Limits").
  Exchange promotes no X-header to a MAPI property unless something already mapped it (Microsoft Exchange Team, "Named Properties, X-Headers, and You"), which is why Outlook's edit dropped `X-Mailypoppins-Draft-Id` in the probe; a message Outlook sends is built from those properties, so an unmapped X-header should not leave with it.
  Proton Bridge stores no custom header (the probe lost `X-Mailypoppins-Draft-Id` at the APPEND, and Proton's feedback forum reports `X-Keywords` stripped the same way), so on Proton a copy reaches the other machine through its HTML, as an edited one does, and the mark still keeps a rebuilt store from writing conflict copies.
  Apple Mail rebuilds a message from its compose window and writes its own X-headers; whether it keeps a foreign one on send is not documented.
  Rejected: the source in an attachment part (every client sends attachments), the source in the HTML encrypted with a key shared between the two machines (a key to distribute and keep, for a gain only on Proton), and no source at all (a reply's quote and the frontmatter would round-trip through `htmd` on every cross-machine pull).
  Accepted risk until the live checks: a header that some client keeps on send carries the draft's frontmatter (without Bcc) and Markdown, whose text the recipients receive anyway.
- Only mp's own mark counts: the last comment of the HTML part, followed by nothing but closing tags; a carried draft whose `server_message_id` is not the copy's `Message-ID`, or whose source does not hash to the mark, is read as a foreign copy; and the HTML a reply or a forward embeds has every `mp:copy` and `mp:source` comment renamed when its companion is written and when it is rendered (review finding B3).

- Approval travels: a draft approved on one machine is approved on the other (Sylvain, 2026-10-23; `draft_mirror::approval_travels()`).
  The status rides inside the carried source, so it is read only from a copy whose mark, source hash and `Message-ID` hold together as above, never from a header of its own: no `X-Mailypoppins-Draft-Status` is written, since a header any client could have set would read the same on a copy mp never wrote.
  A pull or an import takes the carried status only when the carried source reproduces the pushed file (the Bcc put back gives the file's hash), and every attachment is here or carried by the copy: an attachment file that is here holds the bytes of the copy's part of the same name, line ends aside, and any other entry is a part the copy carries under its name; anything else lands as `status: draft`.
  An edit in Outlook, Apple Mail or the Proton app drops the mark, so it demotes the draft on both machines.
  On Proton the approval does not travel: Bridge drops the `X-Mailypoppins-Draft-Source` headers at the APPEND, the other machine reads the copy through its HTML, which is a lossy conversion with no validated status, and the draft lands as `status: draft` to be approved again there.
  Rejected for Proton: the status in the mark comment, which no source validates and any client could write, and which would approve a body converted from HTML rather than the one approved.

## Rollout steps

1. Probe: `examples/draft_probe.rs`, run against `tum` and `proton` on 2026-10-20, with "Probe results" and "Decisions" filled in.
   Done.
2. Contract: `server_message_id` and `conflict_of` in `EmailFrontmatter`, the `draft_sync` table in `src/store/schema.rs`, the `server` member of `DraftRow` and its fixture, `docs/daemon-protocol.md`.
   Done: the table is `src/store/draft_sync.rs`, created additively at open; the `server` member is `mp_protocol::state::DraftServer` on the snapshot row, `draft.changed` and the `draft.list` row.
3. Push: `build_server_draft` and the shared MIME assembly, `ServerOp::PushDraft` and `DeleteDraft`, the drain's APPEND, UID read-back, UID delete and `draft_sync` write, the in-memory copy of `draft_sync` and the watcher hand-off in `src/daemon/watch.rs`, the switch in `config.toml`.
   Done: `src/draft_mirror/` (`push.rs`, `folder.rs`), `send::build_server_draft` over `send::draft_mime_body`; the in-memory copy is the watcher's `MirrorState`, which the runtime re-reads from the store after every drain that ran, and the hand-off is the watcher asking the runtime for a drain, whose mutation hook queues every owed push (`push::enqueue_owed`) before it drains.
4. Send and discard: `outbox.draft_copy`, the `DeleteDraft` in both transitions out of `pending_send`, `draft.discard` and `settle_sent_draft` moving into `.retired/`.
   Done, with one deviation: the draft copy rides the outbox row's envelope as a `draft-copy:` line rather than a new `outbox.draft_copy` column, because the envelope is already the row's extensible record and is carried across a store rebuild whole. `retire_draft_of_row` (the resume path) and the `--sent` sweep retire into `.retired/` too, and every drain point prunes `.retired/` after 30 days counted from the retirement.
5. Reconcile and pull: `messages.draft_marker`, the eviction exemption, the reconcile pass and its recovery of a lost `draft_sync` row, the HTML-to-Markdown step with the `htmd` dependency, conflict copies, the marker import from the other machine.
   Done (`src/draft_mirror/reconcile.rs`, run by the account runtime after every sync body, IMAP accounts only), with three deviations from the design above:
   - The Drafts folder is read directly by the reconcile, one `SELECT` and the six headers it links a copy by (`draft_mirror::folder::HEAD_FIELDS`), a body only for a copy it pulls or imports, and is not ingested into the store; so there is no `messages.draft_marker` column and no eviction exemption, the folder keeps the catalogue's `ignore` policy, and no received list, search or conversation can show a server draft beside its file.
   - A copy mp writes ends its HTML part with a mark of two hashes and carries the draft file, its Bcc left out, in `X-Mailypoppins-Draft-Source` headers (`draft_mirror::carrier`, "Decisions").
     HTML comments survive mp's own APPEND on both servers and no foreign edit (the probe), so a copy that still ends with the mark is exactly a file mp pushed: the recovery of a lost row compares the mark's hash with the file, and mp on the other machine reads a draft back byte for byte when the headers travelled.
     A copy without the mark was edited elsewhere and goes through `parse::draft_html_to_markdown` (`htmd` 0.5.6); the recovery renders it as a pull would and compares that with the file.
   The pull waits for an open outbox row and for an armed undo-send hold, which the daemon's hold scheduler records in `draft_mirror::holds` (a `send.approved` hold covers every approved draft of its account).
   A pull hashes the file again just before its rename and leaves a draft saved since it was read to the next reconcile, and the row records the hash of the bytes the pull wrote.
   A push that finds copies and no row of its folder runs the reconcile instead of pushing, so a rebuilt store never replaces a copy edited elsewhere.
   Two copies under one draft where the row's is the older is another machine pushing over this one: the newer wins, and the local text becomes a conflict copy only when the file moved since the row and is not the newer copy.
   Word's fake list bullets are turned into `- ` items after the conversion rather than by an `htmd` handler.
6. Foreign drafts: the import, and the received list hiding the `drafts`-role folder in the TUI and the desktop.
   Done: `reconcile::import_foreign`; a push that finds a copy its row does not name (an edit in Outlook or another machine's push since the last reconcile) runs the reconcile instead, and an older copy edited elsewhere is kept as a conflict copy before it is deleted, so no text typed in another client is buried under a local push; a reply started elsewhere keeps its quoted thread below a quote marker, so a later pull keeps it. The `drafts`-role folder was already out of both clients' received lists, since #0141 ignores it and both render `mailbox.list`; it now stays ignored whatever an override says, with the reason "mirrored into the Drafts view, read by the drafts mirror".
7. Graph, once an account uses it.

Each step lands on its own and leaves the tree green; until step 3 nothing reaches the server.

## Live checks for Sylvain

The build was verified offline only, against an in-memory Drafts folder; nothing reached a live account.
Installing it turns the mirror on for `tum` and `proton` at once, so the first drain pushes every draft in each account's drafts directory; set `[accounts.drafts] mirror = false` on an account first to hold it back.

- [ ] `mp new` a draft on the Mac; within a few seconds it is in Outlook's and Apple Mail's Drafts with its To, Cc, Bcc, subject, body and attachment, and the Drafts list shows it `synced`.
- [ ] Edit it in Neovim and save; the Drafts folder holds one copy afterwards, and Neovim shows no "file changed" prompt.
- [ ] Edit it in Outlook (a word and a recipient); after the next quick tick the file has both, and the quote below `<!-- mp:quote -->` is unchanged.
- [ ] Edit it in Neovim and in Outlook between two ticks; the draft takes Outlook's text and `<name>.conflict.md` holds yours.
- [ ] Edit it in Apple Mail (a new `Message-ID`); the file follows the edit rather than a second draft appearing.
- [ ] Approve a draft, edit it in Outlook; after the tick it is `status: draft` again.
- [ ] On `tum`, approve a draft on the Mac; within a minute it is `approved` on the home server, and an Outlook edit of it makes it `draft` on both.
- [ ] On `proton`, approve a draft on the Mac; on the home server it stays `draft`.
- [ ] `mp send` a draft; its copy leaves Outlook's Drafts and the file is in `drafts/.retired/`.
- [ ] On `tum`, with an approved draft on both machines, run `mp send-approved` on the Mac and then on the home server; the home server refuses the draft as sent on another machine, and the recipient gets one message.
- [ ] On `tum`, send a draft from the Mac, then within a minute press `x` on it in the home server's TUI; the send is refused as sent on another machine, nothing reappears in Outlook's Drafts, and the file retires within two minutes.
- [ ] Send a draft from Outlook, and delete one in Apple Mail; after two ticks both files are in `drafts/.retired/` and neither comes back.
- [ ] On `tum`, open an mp draft's copy in Apple Mail with View, Message, Raw Source, without editing it: it carries `X-Mailypoppins-Draft-Source` headers and its HTML ends with `<!-- mp:copy v2 ... -->`, and the base64 of the headers decodes to the draft with an empty `bcc:`.
- [ ] On `proton`, a draft made on the Mac appears on the home server with its text and Bcc, through its HTML when Bridge dropped the headers, and a store rebuild there leaves it `synced` with no conflict copy.
- [ ] Send an mp draft unedited from Outlook, from Apple Mail and from the Proton app to an outside address you can read; the received message's source carries no `X-Mailypoppins-Draft-Source` header and no Bcc address.
- [ ] Type half a YAML line into a mirrored draft's frontmatter in Neovim, save, wait two ticks, then fix it; the draft stays in `drafts/` on both machines and the server keeps one copy.
- [ ] Start a draft in Outlook and one in the Proton app; both appear in mp's Drafts view as Markdown files, and no server Drafts mailbox is listed.
- [ ] On the home server, the Mac's draft appears under the same id after a tick there, and an edit there reaches the Mac.
- [ ] Stop the daemon, delete `accounts/tum/store.sqlite3`, start it and let it sync; every draft is `synced` again, with no second copy and no conflict copy.
- [ ] Delete the probe's leftovers listed under "Leftovers to delete", or confirm the first reconcile imported them as drafts and discard them in mp.

## Acceptance criteria

- [x] The probe has run on `tum` and `proton` with Outlook and Apple Mail, and "Probe results" records every item of the list above.
- [ ] A draft created with `mp new` on the Mac appears in Outlook's and Apple Mail's Drafts within one quick tick, with its recipients, Bcc included, its subject, its body and its attachments.
- [ ] An edit saved in Neovim replaces the server copy, and the Drafts folder holds one copy of the draft afterwards.
- [ ] After its first push, a push leaves the draft file's bytes unchanged, so Neovim shows no "file changed" prompt.
- [ ] Deleting the store file and restarting the daemon leaves every synced draft `synced`, with no second copy on the server and no conflict copy.
- [ ] An edit made in Outlook reaches the local file at the next quick tick, with the quoted section below `QUOTE_MARKER` unchanged.
- [ ] The same draft, created on the Mac, appears in the home server's `drafts/` and converges back after an edit there.
- [ ] Editing a draft both in Neovim and in Outlook between two syncs leaves the server's version in the draft and the local text in `<name>.conflict.md` with a fresh `id:`.
- [ ] Sending a draft with `mp send` removes its server copy; a failed delete retries and the copy is gone after the next sync that reaches the server.
- [ ] Sending or deleting a draft in Outlook moves the local file to `drafts/.retired/`, and it is not imported back.
- [ ] A draft started in Outlook appears in mp's Drafts view as a Markdown file, and the server Drafts folder no longer lists as a received mailbox.
- [ ] A pull of a copy edited elsewhere into an approved draft leaves it `status: draft`, and an approval made in mp on the other machine arrives with the draft on an account whose server keeps mp's headers.
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
  - `docs/architecture.md`: the invariant at `:11-12`, the `drafts` and `pending_ops` store bullets at `:227-228` and a `draft_sync` bullet beside them, the Send section at `:279-307` and the retirement at `:299`;
  - `docs/daemon-protocol.md`: the `draft.*` family at `:661`, the draft row at `:212`, `draft.changed` at `:1060`, and the protocol changelog at `:1229`;
  - `docs/daemon-operations.md`: the draft watcher at `:310`;
  - `docs/parity-matrix.md`: LST-01 at `:262`, DFT-02 at `:609`, DFT-12 at `:712`, and a new DFT row for the mirror;
  - `website/src/pages/draft-format.astro` (the frontmatter keys and "After Sending", `:145-160`), `website/src/pages/commands.astro:282` ("Drafts are the only local-only data") and `website/src/pages/config.astro` for the switch;
  - `CHANGELOG.md` and `BACKLOG.md`.

## Out of scope

- Two clients sending the same draft at once, mp and Outlook: neither can see the other's submission, and the server copy's delete does not prevent it.
- Mirroring signatures or the `event:` block of an invite draft.
- A three-way merge of conflicting edits; the conflict copy is the whole answer.
- Drafts of an account with no server Drafts folder.
