---
id: 0142
title: Conversation list, one row per conversation in every mailbox
type: feature
priority: next
status: done
created: 2026-10-13
---

Sylvain decided the behaviour on 2026-10-13: an Outlook-style conversation list, on by default, toggled per account, with local drafts joining the conversation they answer.
Threading stays the header-based `thread_id` ingest assigns once; this ticket adds the grouped list and leaves the `T` overlay of [#0008](0008-threading-conversation-view.md) and its follow-up [#0101](0101-conversation-view-collapse-inline-nav.md) in place.

## Problem

Every mailbox lists one row per message.
A conversation of an inbox message, its reply in Sent and the draft of the next reply is three rows in three places: the inbox shows the question, Sent shows the answer, Drafts shows the draft, and only the `T` overlay puts them side by side, one message at a time.
Outlook shows the same exchange as one inbox row that expands to the inbox message, the user's draft reply and the two sent replies, each labelled with its folder.

Ingest gives every row a `thread_id` once, from `In-Reply-To` and `References` (`resolve_thread_id`, `src/ingest.rs:575`, called at `:352`), and `THREAD_INDEX` serves `(account, thread_id, date_sort)` (`src/store/schema.rs:373`, created at `:389`).
`message.thread` (`src/daemon/methods/message.rs:928`) answers one conversation across mailboxes through `read::thread_messages` (`src/store/read.rs:480`), which folds a `Message-ID` held in several mailboxes into one entry.
What is missing is a listing that groups a mailbox by conversation, a draft that knows which conversation it belongs to, and a client row that expands.

The drafts index has no column for the message a draft answers: `drafts` holds `to_`, `cc`, `subject`, `date` and `snippet` (`src/store/schema.rs:262`), while the frontmatter carries `in_reply_to` and `forwarded_from` (`crates/mp-core/src/types.rs:364` and `:369`, written at `crates/mp-core/src/draft.rs:678` and `:793`).
Graph rows thread on nothing: the Graph pull ingests with `raw: None` (`src/graph.rs:1846`), so `threading_headers` never runs (`src/ingest.rs:256`) and each Graph message roots its own thread.
Replies sent from mp thread on nothing either: `build_draft_message` sets only `From`, `Subject` and a fresh `Message-ID` (`src/send.rs:2829-2832`), so the Sent copy ingests without `In-Reply-To` or `References` and roots its own thread.
PERSO-137 adds the two headers on send and is a prerequisite of this ticket; step 7's re-thread of stored rows covers the replies sent before it.
Once [#0143](0143-server-drafts.md) mirrors drafts to the server Drafts folder, the server copy of a draft is a `messages` row in the `drafts`-role mailbox with the thread's headers, while the local file joins through `include_drafts`.
To keep a draft from showing twice, `list_conversations`, `count`, `date_sort`, `mailboxes` and `thread_messages` exclude the rows of the account's `drafts`-role mailbox; the local file is the one draft member, and a push never re-sorts the list.

## Proposal

A new streamed operation, `message.list_conversations`, lists one mailbox as conversation rows, on the mechanics of `message.list_stream` (#0138).
`message.thread` gains two optional parameters and stays the expansion read.
The clients render a conversation row, expand it in place, and remember the mode per account.

```text
message.list_conversations {account, mailbox}
        -> {operation_id, account, mailbox, total}             total = conversations
   streams  notification `message.conversations`,
            params {offset, operation_id, rows: [ConversationRow]},
            on the calling connection only, chunks of ROWS_CHUNK_BYTES
   settles  operation.finished {operation_id, state, result: {account, mailbox, total}}

ConversationRow {
  count, date_display, date_sort, flagged, has_attachments, has_draft,
  latest: MessageListRow, mailboxes: [str], member_ids: [i64],
  participants: [str], thread_id, unread
}

message.thread {account, row_id | id | selector, mailbox?,
                prefer_mailbox?: str, include_drafts?: bool}
        -> {account, thread_id, subject, messages: [ThreadMessage]}
           ThreadMessage gains draft_id: str|null
```

### A new method, and why not a `group` parameter

- The method is `message.list_conversations`, after the `message.list_stream` and `message.search_server` pattern of a suffix naming how a `message.list` twin differs.
- A `group: "conversation"` parameter on `message.list_stream` was rejected.
  An older daemon answers an unknown parameter with `-32602` at the first mailbox open, where a new method name lets a client refuse the daemon at the handshake with `capability_missing`, the reason #0138 gave for a new method.
  `message.rows` is typed: `MessageRowsChunk.rows` is `Vec<MessageListRow>` (`crates/mp-protocol/src/listing.rs:180`), and `mp-client` decodes a chunk straight from the frame bytes by its prefix (`MESSAGE_ROWS_FRAME_PREFIX`, `:193`, PERSO-106); a second row shape under the same notification would put a type switch into that fast path.
  And `MessageListStreamStarted.total` (`:158`) is documented as the row count of the mailbox, which a grouped stream would change meaning under the same name.
- The answer reuses `MessageListStreamStarted` as it is, with `total` the conversation count: same four members, same meaning of "rows this stream carries".
- The notification is `message.conversations`, a fifth notification method beside `state.event`, `state.resync_required`, `daemon.stopped` and `message.rows` (`docs/daemon-protocol.md:1197`), with params `ConversationRowsChunk {offset, operation_id, rows}`.
- One frame for the whole answer is out for the reason it was in #0138: a conversation row is a `MessageListRow` plus about 200 bytes, and the `mkfixture` mailbox of 50 000 rows has no threading headers (`examples/mkfixture.rs:173` ingests with `raw: None`), so it is 50 000 one-message conversations of about 700 bytes, some 35 MB against the 16 MiB `MAX_RESPONSE_BYTES` (`crates/mp-protocol/src/lib.rs:52`).
- The stream keeps every rule of #0138 unchanged: the whole read before the answer, `client_scoped`, 1 MiB chunks, contiguous offsets, the oversized-row failure, the writer's order and the cancel behaviour.
  `ChunkEncoder` already pushes any `Serialize` row (`src/daemon/methods/message.rs:725`); its `open` hard-codes `METHOD_MESSAGE_ROWS` (`:704`), which becomes a constructor argument.
- The capability is the method name, and the TUI (`TUI_REQUIRED_CAPABILITIES`, `src/daemon/client.rs:315`) and the desktop (`REQUIRED_CAPABILITIES`, `clients/desktop/src-tauri/src/connector.rs:89`) require it at the handshake from step 4 and step 5 on.
- [list-transfer.md](../baselines/decisions/list-transfer.md) holds: the whole list once per open, kept current by events; the conversation listing is one more whole list on the same stream.

### The conversation row

A new `mp_protocol::listing::ConversationRow`, fields declared in key order so the hand-built frame equals `frame::encode` of the same value, as `WireRow` does.

- `thread_id`: the `messages.thread_id` the members share, which is what `message.thread` and the client's selection hold the row by.
- `date_sort` and `date_display`: the newest member's, across every mailbox, drafts excluded.
  A reply sent today lifts an inbox conversation to the top of the inbox, as Outlook does; a draft being edited does not reorder the list.
- `latest`: the newest copy of a member in the listed mailbox, a `MessageListRow` byte for byte.
  It is the row the preview shows for a collapsed conversation, and its `subject` is the row's subject.
- `member_ids`: the `messages.id` of every member row in the listed mailbox, newest first, duplicates of one `Message-ID` included, since each is a server copy an action has to reach.
- `count`: the conversation's messages across every mailbox, one per `Message-ID`, drafts excluded.
- `unread`: the `Message-ID`s with an unseen copy in the listed mailbox.
  Counting only the listed mailbox keeps the number in step with what marking the row read does, which acts on `member_ids` alone.
- `participants`: the `From:` headers of the members, verbatim, one per address compared case-insensitively, newest first, at most five, so a 300-message mailing-list thread stays a small row; the client extracts display names as it does for a list row.
- `mailboxes`: the distinct mailboxes the members live in, sorted, drafts not included.
- `has_attachments`: any member, anywhere, has one.
- `flagged`: any member in the listed mailbox is flagged, the same members the flag action on the row toggles.
- `has_draft`: a draft in the index answers or forwards a member.

The stream is ordered by `date_sort DESC`, then by `latest.id DESC`.
The sidebar counts stay per message: the `counts` invalidate and `mailbox_read_counts` (`src/store/read.rs`) are not touched.

### The store query

`read::list_conversations(store, account, mailbox)` beside `list_mailbox_dated` (`src/store/read.rs:209`), in two statements on one snapshot.

1. The mailbox's rows, each with whether its conversation has another member:
   `SELECT <the list_mailbox_sql columns>, EXISTS (SELECT 1 FROM messages AS other WHERE other.account = ?1 AND other.thread_id = messages.thread_id AND other.mailbox <> 'drafts' AND other.id <> messages.id) ... WHERE messages.account = ?1 AND messages.mailbox = ?2 ORDER BY messages.date_sort DESC, messages.id DESC`.
   The walk is the flat listing's, over `messages_list` (`src/store/schema.rs:242`), and the probe is index-only on `messages_thread_mailbox` (`(account, thread_id, mailbox)`), an additive index a store gains on its next open.
   A row with no other member is a conversation of one and is folded as it stands.
   That includes a row with a `NULL` `thread_id`, keyed by its `Message-ID`, which `message.thread` already treats as its own conversation (`src/daemon/methods/message.rs:1177`).
2. The members of the conversations step 1 found shared, their `thread_id`s bound as one JSON array:
   `SELECT <the same columns> ... WHERE messages.account = ?1 AND messages.mailbox <> 'drafts' AND messages.thread_id IN (SELECT value FROM json_each(?2)) ORDER BY messages.thread_id, messages.date_sort, messages.id`.
   It probes `messages_thread` (`(account, thread_id, date_sort)`) once per conversation in index order, so each conversation is folded as its rows arrive.
   The listed rows of a shared conversation are read a second time here, because holding them from step 1 until their other members arrive made the fold walk rows out of the cache and cost more than the second read.

A query-plan test pins both statements to index walks with no temp B-tree, on the precedent of `the_listing_is_served_by_the_messages_list_index`.

The fold runs in Rust over the ordered rows, one conversation at a time.
A `Message-ID` seen twice in one conversation counts once in `count`, the rule `thread_messages` applies (`src/store/read.rs:847`), and is unread when any of its copies in the listed mailbox lacks `\Seen`.
Keeping the dedup in Rust keeps one spelling of the rule for the list and the expansion; `COUNT(DISTINCT message_id)` in SQL would be a second one.

No summary table and no materialised count in this ticket.
Membership and flags change through ingest, the five mutation methods, the prune, eviction and reconcile, and a materialised count kept in step by every one of those writers would show a wrong unread number without any error the day one of them forgets.
The read is a bounded multiple of the flat listing's: on the 50 000-row fixture every row is its own conversation, so step 1 is the flat listing's walk plus 50 000 index-only probes and step 2 reads nothing.
The flat stream's daemon side is 41 ms of read on the baseline host and 111 ms in all on the home server ([message-list-unbounded.md](../baselines/message-list-unbounded.md)); the target is under twice the flat stream's figure on the same host and fixture, and a table is reopened only if the bench misses it.
`mkfixture` gains an option to give the `Bulk` mailbox threaded rows with replies in Sent, so the bench measures the fold on conversations of several members as well as the singleton worst case.

### Drafts in a conversation

A draft joins the conversation of the message it answers or forwards, by `Message-ID`.
The frontmatter value is the source row's `messages.message_id` copied verbatim (`source_from_row`, `src/draft.rs:66`), and an exact string match finds it.

- `drafts` gains two nullable columns, `in_reply_to` and `forwarded_from`, filled by `row_for` (`src/store/drafts.rs:401`) and written by `refresh_reporting` (`:130`).
- The columns are added by an `ALTER TABLE ... ADD COLUMN` on open, beside `ensure_additive_indexes` (`src/store/schema.rs:403`), with no `SCHEMA_VERSION` bump (`:40`).
  A bump makes an existing store unusable until it is rebuilt from the server, and the drafts table needs none of that: `refresh_reporting` deletes and rewrites an account's rows on every refresh, at engine start among other times, so the new columns fill at the next refresh with nothing to migrate.
- After the fold, the daemon reads the account's drafts with either column set, tens of rows, resolves each `Message-ID` through `messages_message_id` (`read::find_by_message_id`, `src/store/read.rs`) to its `thread_id`, and sets `has_draft` on that conversation when the listing holds it.
- A draft with `status: sent` is left out, since the Sent copy ingest threads stands for it.
- A draft never makes a conversation appear in a mailbox: a conversation is listed where one of its messages lives, and Drafts is not a mailbox `message.list_conversations` accepts, as it is refused by `message.list` today (`resolve_mailbox`, `src/daemon/methods/message.rs:2182`).
  The Drafts view stays the flat `draft.list`.

### Expansion through `message.thread`

`message.thread` stays the one conversation read, with two optional parameters; without them its answer is unchanged and the `T` overlay is not affected.

- `prefer_mailbox`: when the fold of `thread_messages` meets several copies of one `Message-ID`, it keeps the copy in this mailbox and otherwise the first row as today.
  The expanded inbox conversation then names the inbox copy, and `Enter` on it opens the row an action on the collapsed row would have touched.
- `include_drafts`: the drafts that join the conversation, as members ordered by their file's `mtime` among the messages.
  `ThreadMessage` (`crates/mp-protocol/src/listing.rs:233`) gains `draft_id: Option<String>`; a draft member carries its draft `id` there, `id: 0`, `mailbox: "drafts"`, an empty `message_id`, the account's address as `from`, and its frontmatter `date` as `date_display`.
- `thread` does not call `only_params`, so an older daemon would ignore both parameters without an error.
  That cannot reach a client: both ship in step 2 with `message.list_conversations`, which the clients require at the handshake.
- The expanded list is newest first, the order of the mailbox around it (each client reverses `message.thread`'s oldest-first answer), each member labelled with its mailbox (`Inbox`, `Sent`, `Archive`, `Drafts`, or an extra mailbox's label) whatever mailbox the list shows.

### Unread and actions on a conversation row

- On a collapsed row, archive, delete, move, flag, mark read and mark unread act on the members living in the listed mailbox, the ids in `member_ids`, and on nothing in Sent, Archive or Drafts.
  Each client issues the existing per-message mutation for every id, as it already does for a multi-selection.
- A conversation whose last member here was archived or deleted leaves the listing; its members elsewhere stay where they are.
- On an expanded member, every action applies to that member alone, in whatever mailbox it lives, and to a draft member the Drafts actions apply.
- A multi-selection of conversation rows is the union of their `member_ids`.

### Keeping the list current

The flat list folds row deltas in place (`apply_row_delta`, `clients/tui/src/queries.rs:291`), and a conversation list cannot.
A delta about Sent changes a conversation listed in the inbox, and the daemon publishes no `message.row` at all: a message change reaches clients only as a `counts`-scope `state.invalidate` of `mailbox:<account>/<mailbox>` (`Event::from_change`, `src/daemon/state/events.rs`).

- In conversation mode a client re-streams the listing on a `counts` invalidate of any of the account's mailboxes, on any row delta `MessageRowDelta::decode` (`crates/mp-client/src/queries.rs`) yields, on `draft.changed` and on a `state.remove` of a `draft:` or `message:` resource.
  The flat list keeps its rule, where the `counts` scope drives only the sidebar.
- The re-stream is coalesced: at most one stream in flight and one owed, so a sync burst of hundreds of deltas costs two streams.
- The user's own action updates the row at once, in the pending state both clients already draw, and the re-stream confirms it.
- A targeted refresh of the touched conversations alone is a follow-up, listed under Out of scope.

### `mp-client`

- `mp_protocol` gains `METHOD_MESSAGE_LIST_CONVERSATIONS`, `METHOD_MESSAGE_CONVERSATIONS`, `ConversationRow`, `ConversationRowsChunk` with its own frame prefix and `from_frame_line`, and `ConversationListing {account, mailbox, total, conversations}`, in `crates/mp-protocol/src/listing.rs` beside `MessageListing` (`:127`).
- The session thread's collector (`RowsCollector`, `crates/mp-client/src/session.rs:1001`) becomes generic over the chunk type; the conversation stream then inherits the deadline, the cancel at the deadline, the contiguity checks and the drop of a stray chunk (`:716`) without a second implementation.
- `Queries` gains `list_conversations(account, mailbox) -> Result<ConversationListing>` and `thread_expanded(account, row_id, mailbox)` for the extended `message.thread`.
  The default body of `list_conversations` refuses with an error naming the method, since no non-streamed method answers it; `Session` and `QueryHandle` override it, and only the fixtures of the tests that exercise conversation mode answer it.

### TUI

- `EmailEntry` (`clients/tui/src/app/types.rs:115`) stays the member row, and the list holds a sequence of conversation headers, each followed by its members when expanded; `list_emails` (`clients/tui/src/queries.rs:68`) gains a conversation twin, and the `LoadMailbox` route (`clients/tui/src/commands.rs:138`) gains `message.list_conversations` and `message.thread`.
- `render_email_list` (`clients/tui/src/ui/list.rs:153`) draws a header row as: the status glyph of the newest unread member (`status_marker`, `:49`), the participants, a `(n)` count when `count` is above one, the attachment, invite, flag and draft markers, the subject of `latest` and the conversation's date; a header with unread members is bold as an unread row is today.
  An expanded member is indented under its header and prints its mailbox label in place of the participants.
  A conversation of one message renders exactly as the flat row does.
- `Enter` on a collapsed header with more than one member expands it and keeps the cursor on the header; `Enter` on an expanded header or on a member reads it, the `FocusBody` it is today (`clients/tui/src/app/keymap.rs:869`), so a conversation of one reads on the first `Enter`.
- `l` expands and `h` collapses the conversation under the cursor, from the header or from any of its members.
  Neither is bound in the list: `h` and `l` are taken only inside the file picker overlay (`clients/tui/src/app/keys.rs:1987` and `:1994`), and `sl` is a chord under the `s` leader.
- The `t` family keeps `tt` for the overlay (`clients/tui/src/app/keymap.rs:1028`), as [#0092](0092-keybinding-scheme-redesign.md) and #0101 assume; `tc` is free and becomes "Conversation view on / off for this account".
- The preview follows the cursor: `latest` for a collapsed header, the member for a member row, the draft preview for a draft member; reading a member from another mailbox opens it by its row id without switching the sidebar's mailbox.
- The list filters, the flagged-only filter and jump-to-date run over conversation rows: a filter matches the participants and `latest`, flagged-only keeps rows with `flagged`, and jump-to-date uses `date_sort`.

### Desktop

- The Tauri `MessageList` (`clients/desktop/src-tauri/src/commands.rs:182`) gains a `Conversations {account, mailbox, total, rows}` variant, which `list_messages_on` (`:774`) answers through `Budgeted`'s `list_conversations` when the account is in conversation mode; `Budgeted` overrides it as it overrides `list_stream` (`clients/desktop/src-tauri/src/session.rs:96`), and the fixture door answers it from its listing, one conversation per message plus a canned thread of an inbox message, a draft and two sent replies.
- A `ConversationRow.tsx` beside `MessageRow.tsx` in `clients/desktop/src/components/list/` draws the header with a disclosure chevron, the participants, the count, the unread count and the badges `MessageRow` draws; a `ConversationMemberRow.tsx` draws a member with its mailbox label.
- `MessageListPane.tsx` maps the visible sequence of headers and expanded members at `:228` instead of the bare rows, and `useWindow`'s fixed `ROW_HEIGHT` (`clients/desktop/src/components/list/useWindow.ts:13`) holds for both kinds of row.
  The selection is held by `thread_id` for a header and by row id for a member.
- `filteredRows` (`clients/desktop/src/app/state.ts:1043`) and `messages_loaded` (`clients/desktop/src/app/reducer.ts:1397`) take the new variant with the same generation guard.
- The keys are the TUI's: `Enter`, `l` and `h` from the generated `keymap.json`, and `tc` for the toggle.
  The desktop's `tt` toggles the reader's text mode (`clients/desktop/src/keymap/catalog.ts:341`) and its conversation entry is badged `later` (`:167`); this ticket leaves both as they are.
- The TS types are regenerated with `cargo test -p mp-protocol --features ts --test ts_bindings`, with every new type listed in `export_into` (`crates/mp-protocol/tests/ts_bindings.rs:48`), since `every_ts_type_is_listed` runs only under that feature ([lessons-learned](../lessons-learned.md), "`every_ts_type_is_listed` only runs under `--features ts`").

### The setting

Conversation mode is on for every account unless the user turned it off, and each client remembers the accounts it shows flat.
The choice is a presentation one, and both clients keep presentation choices out of the daemon: the desktop's `prefs.ts` and `desktop.json` say so in their headers (`clients/desktop/src/app/prefs.ts:1`, `clients/desktop/src-tauri/src/settings.rs:13`), and no `settings.*` method exists.

- The desktop adds `flatListAccounts: string[]` to `Prefs` (`clients/desktop/src/app/state.ts:112`), stored in localStorage with the pane sizes; `desktop.json` holds only string values per `SettingKey`, which a per-account list does not fit.
- The desktop's Settings view gains a "Conversation view" switch per account in its Accounts section (`clients/desktop/src/components/settings/SettingsView.tsx:403`), and the palette gains the entry through the `tc` row of `keymap.json`, like every keymap-derived entry (`clients/desktop/src/components/palette/CommandPalette.tsx:36`).
- The TUI has no settings view (`clients/desktop/src/keymap/catalog.ts:316` records it) and reads `config.toml` once at startup, which it never writes.
  It gains `tui.json` in `mp_core::config::config_dir()`, beside `config.toml`, holding `{"flat_list_accounts": [...]}`, written by the `tc` action and readable by hand; the palette entry comes from the `tc` row of `KEYMAP` through `palette_actions` (`clients/tui/src/app/keymap.rs:1714`).
- The two clients can disagree for one account, as their themes already do; a daemon-held preference shared by both would need a method pair and an event for a choice that is not domain state, and is not proposed.
- An account unknown to the stored list is in conversation mode, so a new account starts grouped.

## Rollout steps

One commit per step, each leaving the tree green.
Until step 6 the clients default to the flat list, so `main` never ships a mode the user cannot leave; step 6 adds the switch and turns the default on.

1. Contract: `ConversationRow`, `ConversationRowsChunk`, `ConversationListing`, the two constants and `ThreadMessage.draft_id` in `mp-protocol`; fixtures `message.list_conversations.{request,response}.json`, `notification.message_conversations.json` and `message.thread.drafts.{request,response}.json` under `crates/mp-protocol/fixtures/`, in a list of their own in `tests/daemon_protocol_fixtures.rs`; the TS bindings; the protocol document's sections, reviewed by Sylvain before any daemon code.
2. Store and daemon: the two `drafts` columns and their fill, `read::list_conversations` with its query-plan test, the `ChunkEncoder` method argument, `message.list_conversations` in an array of its own beside `MESSAGE_STREAM_METHOD_SPECS` (`src/daemon/methods/message.rs:428`), `message.thread`'s two parameters, the `mkfixture` threading option and the bench.
3. `mp-client`: the generic collector, `list_conversations` and `thread_expanded` on `Queries`, the collector tests.
4. TUI: the header and member rows, `Enter`, `l` and `h`, the re-stream on deltas, the handshake requirement, golden frames for a collapsed and an expanded conversation; conversation mode reachable from tests only.
5. Desktop: the `Conversations` variant, the two row components, the windowed sequence, the keys, `REQUIRED_CAPABILITIES`, the fixture door's thread, vitest coverage.
6. The setting: `tc` in `KEYMAP` and both palettes, `tui.json`, `flatListAccounts` and the Settings switch, the default turned on, `keymap.json` and `website/src/data/tui-keys.json` regenerated.
7. Later, in its own ticket: Graph's `conversationId` feeds `thread_id` for Graph rows, and Gmail's `X-GM-THRID` for Gmail rows that carry no usable headers.
   `thread_id` is assigned once at ingest (`src/ingest.rs:352`), so this step also owes a one-shot re-thread of the rows already stored.

### Documents to update when shipping

- `docs/daemon-protocol.md`: "Method kinds" (`:115`) adds `message.list_conversations` to the operations; a section beside "The streamed listing" (`:884`); the `message.thread` section (`:431`) for the two parameters and `draft_id`; "Notification methods" (`:1197`) goes from four to five; "Fixtures" (`:1213`); a paragraph in the protocol changelog (`:1229`).
- `docs/parity-matrix.md`: a new `LST` row for the conversation list beside `LST-10` (`:355`), which stays the overlay.
- [list-transfer.md](../baselines/decisions/list-transfer.md): the conversation listing travels on the same stream; [message-list-unbounded.md](../baselines/message-list-unbounded.md): the conversation bench figures.
- The website: `website/src/data/tui-keys.json` through `scripts/regen-website-keys.sh`, and `website/src/pages/faq.astro:82`, which describes the `t` family.
- `CHANGELOG.md` `[Unreleased]`, and the `BACKLOG.md` line goes when step 6 lands.

## Acceptance criteria

- [x] In the inbox, a conversation of an inbox message, a draft reply and two sent replies is one row with `(3)`, the draft marker and the date of the later sent reply; expanding it lists four members newest first labelled `Drafts`, `Sent`, `Sent` and `Inbox`.
  Evidence: `tests/daemon_conversations.rs` (`an_inbox_message_its_two_sent_replies_and_a_draft_are_one_conversation`) and the desktop's `conversations.test.tsx` (`lists one row per conversation...`, `expands on Enter...`).
- [x] The same conversation is one row in Sent, with the same four members on expansion.
  Evidence: the same daemon test, its Sent half.
- [x] A `Message-ID` held in the inbox and in Archive counts once in `count` and appears once on expansion, as the inbox copy when the list shows the inbox.
  Evidence: the same daemon test, whose question is held in both.
- [x] Archiving a collapsed inbox conversation archives its inbox members and leaves its Sent members in Sent.
  Evidence: `src/tui_tests/conversations.rs` (`actions_on_a_collapsed_row_reach_its_members_here`).
- [x] Marking a collapsed row read clears its `unread` and its bold.
  Evidence: the same TUI test, and the desktop's `conversations.test.tsx` row toggle.
- [x] A message that arrives in a listed conversation, or a reply sent from it, updates the row within one re-stream.
  Evidence: `src/tui_tests/conversations.rs` (`an_arrival_and_a_sent_reply_update_the_row_within_one_restream`, `deltas_restream_the_list_coalesced`).
- [x] The streamed conversation rows equal the fold of the flat listing plus `message.thread` for the same mailbox, in a daemon test over a real socket.
  Evidence: `tests/daemon_conversations.rs` (`streamed_conversations_equal_the_fold_of_the_listing_and_the_thread`).
- [x] No frame of the stream exceeds `ROWS_CHUNK_BYTES` plus one row plus the envelope, on the 50 000-row fixture.
  Evidence: `tests/daemon_conversations_cap.rs` (`no_conversation_frame_passes_the_budget_on_the_large_mailbox`).
- [x] On the 50 000-row fixture the daemon side of `message.list_conversations` is under twice the flat stream's on the same host, recorded in `message-list-unbounded.md`.
- [x] `message.thread` without the new parameters answers byte for byte as before, and the `T` overlay's golden frames are unchanged.
  Evidence: the plain read of `an_inbox_message_its_two_sent_replies_and_a_draft_are_one_conversation` carries no `draft_id` key, `message.thread.{request,response}.json` and `tests/daemon_thread_slice.rs` are unchanged since step 1, and no snapshot but the help overlay's row count moved.
  The overlay had no golden frame of its own then; PERSO-148 added two, `golden_thread_overlay_collapsed` and `golden_thread_overlay_expanded` in `src/tui_tests/conversations.rs`, which now pin it.
- [ ] `tc` and the palette entry switch an account between the conversation list and the flat list in both clients, the desktop Settings switch does the same, and the choice survives a restart of each client.
  Holds here: `src/tui_tests/conversations.rs` (`tc_and_the_palette_switch_the_list_and_tui_json_remembers_it`), `clients/tui/src/prefs.rs`, and the desktop's vitest (`conversations.test.tsx` "the conversation view setting", `settings.test.tsx` "the conversation view switch"); open: a restart of the built desktop app on the Mac.
- [ ] A TUI or desktop build against a daemon without `message.list_conversations` stops at the handshake with `capability_missing`.
  Holds for the TUI: `src/daemon/session.rs` (`a_daemon_without_the_conversation_listing_refuses_the_tui`); open: the desktop's `the_required_capabilities_cover_the_conversation_list` (`clients/desktop/src-tauri/src/connector.rs`), which runs on the Mac only.
- [ ] `cargo test --workspace`, `cargo test -p mp-protocol --features ts --test ts_bindings` and the desktop's vitest run pass; the desktop crate's `cargo test` passes on the Mac.
  Holds on the home server for the first three at step 6; open: the desktop crate's `cargo test` on the Mac.

## Follow-ups after shipping (PERSO-146)

The gaps the step 4 to 6 workers and reviewers left open, each small and none blocking daily use.

- Done: the move picker leaves out the mailbox an expanded member lives in rather than the listed one, in both clients (`moveSources` in `clients/desktop/src/app/actions.ts`, `move_sources` in `clients/tui/src/app/keys.rs`).
- Done: the desktop offers `ce`, `cA`, `cD` and `x` on a draft member, and its reader shows the draft toolbar less Attach file, as the TUI already did; the fields come from a `draft_preview` of the member.
- Done: a TUI draft member takes To, Cc, Bcc, Reply-To, subject, status and a frontmatter `from` from its file (`fill_draft_member`), so the Send confirmation of an approved draft no longer offers to approve it.
- Done: a TUI message member's headers pane prints To and Cc, read off the `message.get` its preview already sends (`MessagePreview` in `mp-client`).
- Done: a TUI conversation list's load sets the sidebar count to the sum of its headers' `member_ids`, the listed mailbox's messages.
- Open, needs a decision: the list filter of both clients matches a conversation by its participants and `latest` only, as this ticket specified, so a conversation whose matching member is neither listed nor among the five participants stays hidden.

## Cross-references

- [#0008](0008-threading-conversation-view.md) (done): the persisted `thread_id`, `thread_messages` and the `T` overlay this ticket reads from.
- [#0101](0101-conversation-view-collapse-inline-nav.md): collapse and inline navigation inside the overlay; it builds on the overlay and is independent of this list.
- [#0092](0092-keybinding-scheme-redesign.md): the `t` family `tc` joins, and the `Enter` reads rule this ticket narrows for a collapsed header.
- [#0100](0100-command-palette.md): the TUI palette derived from `KEYMAP`.
- [#0136](0136-gui-feedback-after-m4.md): the desktop's Settings view and its presentation settings.
- [#0138](0138-message-list-streamed.md): the stream, the chunk budget and the collector this method reuses.
- [#0141](0141-mailbox-auto-discovery.md): more mailboxes make cross-mailbox conversations more common; nothing here depends on it.
- `LST-10` in `docs/parity-matrix.md` and [list-transfer.md](../baselines/decisions/list-transfer.md).

## Out of scope

- `mp list-messages --conversations`: the command pages each mailbox with `-n` over `message.list` (`src/main.rs:360`), and the conversation listing is a whole-mailbox stream with no `limit`; a bounded grouped listing is its own method and its own ticket.
- A targeted refresh: `thread_id` on `MessageListRow` and a `thread_ids` filter on the method would let a client re-read only the conversations a delta touches.
- A reply-depth tree inside a conversation, and inline reply, both already set aside by #0101.
- Grouping in the Drafts view, in local search results and in the merged server search of `LST-08`, which stay flat.
- Conversation-wide actions that reach members in other mailboxes.
- Threading Graph and Gmail rows by their server ids, which is rollout step 7 and a ticket of its own.
