---
id: 0138
title: Stream the whole-mailbox listing past the 16 MiB frame cap
type: perf
priority: next
status: done
created: 2026-10-04
---

Shipped 2026-10-08 on branch `perso-0138-list-stream` in six commits, one per rollout step: the contract (`0ca2f9c`), the daemon (`07a4523`), `mp-client` (`5590583`), the TUI (`8e3f5a2`), the desktop (`feb5280`) and the regression, bench and documents (step 6).
The desktop crate does not build on the Linux host the work was done on, so its `cargo test` and a live open of a 50 000-row mailbox in both clients are still owed on the Mac; the unticked criteria below say which.

Sylvain chose a streamed answer over offset paging, and the same change carries the rows as bytes instead of a `serde_json::Value` tree.
The facts this design builds on are in `.agents/research/2026-10-04-message-list-contract-scout.md`; every file and line cited below was re-read for this ticket.

## Problem

A mailbox past about 34 000 rows cannot be opened in the TUI or the desktop client.
Both open a mailbox with `message.list` and `limit: null`, the whole mailbox in one answer, as [list-transfer.md](../baselines/decisions/list-transfer.md) decided.
The answer costs about 488 bytes a row: 24 387 191 bytes at 50 000 rows of `mkfixture --rows 50000` ([message-list-unbounded.md](../baselines/message-list-unbounded.md)).
`MAX_RESPONSE_BYTES` is 16 MiB (`crates/mp-protocol/src/lib.rs:52`), so the cap falls at about 34 000 rows, fewer with long subjects and recipient lists.
Past it, `encode_capped` (`src/daemon/server.rs:1011`) replaces the answer with `frame_too_large` (`-32004`, `{limit, seen}`) and the client gets no listing at all; the TUI's `LoadMailbox` then shows an empty list (`clients/tui/src/actions.rs:926`).

The method is also slow for what it does.
At 50 000 rows it spends 156 ms, of which 41 ms is the store read and about 115 ms is building and dropping a `Value` object of fifteen keys per row (`ListRead::run`, `src/daemon/methods/message.rs`), plus 22 ms of `frame::encode` on top.
The same borrowed `WireRow`s serialised straight to bytes take 31.5 ms.
`Outcome::result` is a `Value`, so no change to `message.list` alone can skip the tree.

## Proposal

A new operation-kind method, `message.list_stream`, answers at once with the listing's head and then streams the rows to the calling connection in chunks of about 1 MiB, each its own frame.
`message.list` stays, unchanged, for bounded calls.

```text
message.list_stream {account, mailbox}
        -> {operation_id, account, mailbox, total}
   streams  notification `message.rows`, params {offset, operation_id, rows: [...]},
            on the calling connection only
   settles  operation.finished {operation_id, state, result: {account, mailbox, total}}
```

### Naming and capability

- The method is `message.list_stream`, after the `message.list_server` and `message.search_server` pattern of a suffix naming how a `message.list` twin differs.
- The capability is the method name, `message.list_stream`, which the dispatcher-derived list advertises as soon as the method is registered (`docs/daemon-protocol.md:78`).
- It is a new method rather than a changed `message.list`, as `signature.list` and `draft.attach` were: a client that requires it gets `capability_missing` from an older daemon at the handshake instead of a wrong or refused answer at the first mailbox open.
- `since: 1`; the protocol range stays `{min: 1, max: 1}`, since the change is additive.
- The spec goes in an array of its own, `MESSAGE_STREAM_METHOD_SPECS` in `src/daemon/methods/message.rs`, for the reason `MESSAGE_HTML_METHOD_SPECS` is one.

### Parameters and the immediate answer

- `account` and `mailbox` are resolved exactly as `message.list` resolves them, and refused the same way: `account_unknown`, `account_not_ready`, `-32602` naming the known mailboxes, Drafts refused.
- There is no `limit` and no `projection`: the method exists for the whole mailbox in the `list` projection, and a bounded or envelope listing is `message.list`.
- The handler reads the whole mailbox before it answers, on the blocking pool as `list_off_thread` does today, so every refusal is the call's own error and no operation id is issued for a call that cannot succeed; this is the `sync.watch` precedent of validating before the id (`docs/daemon-protocol.md:792`).
- The answer is `{operation_id, account, mailbox, total}`, where `total` is the row count of that one read and therefore exactly the number of rows the stream will carry.
  An operation answer with members beside `operation_id` has a precedent in `send.draft`, which answers `{operation_id, held: true}` when it armed a hold (`docs/daemon-protocol.md:1237`).

### The `message.rows` notification

- Each chunk is a JSON-RPC notification of method `message.rows`, params `{offset, operation_id, rows}`, a fourth notification method beside `state.event`, `state.resync_required` and `daemon.stopped` (`docs/daemon-protocol.md:1114`).
- `rows` are `message.list` rows, the `WireRow` shape byte for byte, `selector` and `date_sort` included, so a client decodes them with the code it has.
- `offset` is the position of the chunk's first row in the stream, not a request parameter: [list-transfer.md](../baselines/decisions/list-transfer.md)'s "no `offset` parameter" stays true, because nothing lets a client ask for a page.
- It is not a `state.event` kind, unlike `message.server_hit`:
  A lifecycle event reaches every bootstrapped connection, so one mailbox open would push 24 MB to every client.
  The per-connection outbound queue holds 4 MiB (`src/daemon/state/events.rs:285`), so five queued chunks overflow it: the client's domain events are discarded and it is told to resync, and a chunk that no longer fits after the discard is dropped, which would leave a hole in the rows.
  Every event takes a revision from the dense counter, which a row chunk has no use for.
  `Event::Lifecycle` carries its payload as a `Value`, which is the tree this change removes.

### Chunk size

- A chunk closes once its encoded rows reach `ROWS_CHUNK_BYTES = 1 << 20` (1 MiB), so every frame is at most 1 MiB plus one row plus the envelope, sixteen times under the cap.
- At 488 bytes a row that is about 2150 rows a chunk and 24 frames at 50 000 rows.
- A byte budget rather than a row count, because row size varies with subjects and recipient lists, and the cap is in bytes.
- The last chunk may be short; a mailbox of zero rows streams no chunk at all and settles with `total: 0`.
- A single row whose own encoding exceeds `MAX_RESPONSE_BYTES` minus the envelope fails the operation with `-32004` `frame_too_large` and `{limit, seen}` in the finished event's `error`, instead of writing a frame the client's decoder (`crates/mp-client/src/connection.rs:84`) would cut the connection on.
  This is the one place `encode_capped`'s rule applies to the stream: the reply path checks a whole answer, and `encode_outgoing` (`src/daemon/server.rs:804`) checks no event at all, so the chunk encoder carries its own check.

### Ordering

- Rows travel newest first, the order `read::list_mailbox_dated` returns today; no client re-sorts.
- Offsets are contiguous: chunk *n+1*'s `offset` is chunk *n*'s `offset` plus its row count, starting at 0, and the last chunk ends at `total`.
- On the calling connection no `message.rows` frame of an operation follows that operation's `operation.finished`.
  For `succeeded` and `failed` every chunk precedes the finish, by the order the producer and the writer keep below.
  For `cancelled` the finish can come first, since `OperationRegistry::cancel` (`src/daemon/operations.rs:604`) fans it out the moment `operation.cancel` is dispatched, while chunks may still sit in the channel; the writer drops those, so a cancelled stream delivers some prefix of the rows, possibly none, and nothing after its finish.
- `state.event` frames from other changes may interleave between chunks; the delta-versus-listing race is no wider than today's, where the TUI's result channel and its event channel are not ordered against each other either.

### Daemon mechanics

- `ClientCtx` (`src/daemon/dispatch.rs:90`) gains a `rows` sink, the sending half of a bounded per-connection channel of encoded frames, each paired with a clone of its operation's cancel token, capacity four chunks (4 MiB, the outbound queue's byte budget), so a method can address the connection that called it.
- `serve_connection` (`src/daemon/server.rs:598`) gains the receiving half and picks the next frame in the order replies, then row frames, then the outbound queue.
  The worker publishes `operation.finished` only after its last chunk is in the channel, and the writer prefers the channel to the outbound queue, which is what puts every chunk ahead of a `succeeded` or `failed` finish on that connection.
  When the writer picks a row frame it checks the frame's token first and drops the frame if the token is shut.
  `settle_cancelled` (`src/daemon/operations.rs:670`) shuts the token before it builds the finished event and before `cancel` fans that event out, so any chunk the writer picks after a `cancelled` finish is queued sees the shut token and is dropped; a chunk picked earlier is written before the finish can be.
- The worker is a `spawn_blocking` producer that holds the owned rows of the read, encodes one chunk at a time and `blocking_send`s it, so the encoding stays off the runtime workers as `list_off_thread` keeps the read off them, and a client that stops reading parks the producer instead of growing the daemon's memory.
- The frame is built as bytes: the envelope prefix `{"jsonrpc":"2.0","method":"message.rows","params":{"offset":N,"operation_id":"…","rows":[`, each borrowed `WireRow` written with `serde_json::to_writer`, commas between, and `]}}\n`.
  Keys are in sorted order, so the bytes equal what `frame::encode` of the same notification as a `Value` produces and the fixture test can compare the two.
  A `RawValue` per row would add an allocation per row for nothing the hand-built envelope does not already give.
- The token is observed between chunks.
- `cancel_scope` is `client_scoped`: the rows are addressed to one connection, and once it is gone nobody can read them.

### Cancel and failure

- `operation.cancel` settles the operation `cancelled` with the table's `-32008` `{operation_id}` at once, as every operation does; the producer stops at its next chunk boundary, and the writer drops the chunks still in the channel.
- A disconnect of the calling connection cancels it the same way, by its `client_scoped` scope.
- Since the store read happens before the answer, a stream cannot fail on the store; what remains is a cancel, a disconnect, a daemon shutdown and the oversized row above.
- A client treats any finish other than `succeeded` as "no listing": it discards the partial rows and keeps the list it held, and in step 1 it never shows a partial list.
- A client that receives rows with a gap in `offset`, rows past `total`, or a success whose rows do not sum to `total` reports a protocol error and keeps its old list.

### What stays on `message.list`

- `message.list` with a bounded `limit` stays as it is, for `mp list-messages` (`src/main.rs:1423`), whose `-n` caps each mailbox.
- The envelope projection stays on `message.list` for `mp dump-mailbox` (`src/main.rs:1556`).
- `limit: null` stays accepted and keeps the cap, so a mailbox past 34 000 rows still answers `frame_too_large` there.
  Refusing it would break an older TUI or desktop build talking to a newer daemon for every mailbox, where keeping it breaks nothing that works today.
  The protocol document marks `limit: null` as the small-mailbox path and points at `message.list_stream`.

## Client changes

### `crates/mp-client`

- `mp_protocol` gains `METHOD_MESSAGE_ROWS`, `MessageListStreamStarted {operation_id, account, mailbox, total}` and `MessageRowsChunk {offset, operation_id, rows: Vec<MessageListRow>}`, in `crates/mp-protocol/src/listing.rs` beside `MessageListing` (`:127`).
- The session thread (`crates/mp-client/src/session.rs`, `serve`) gains a streamed call: it sends `message.list_stream`, then reads notifications until the contiguous rows of that id reach `total` or the `operation.finished` of that id arrives, folding each `message.rows` of that id into one `Vec<MessageListRow>` and publishing every other notification as it does today, the finish included, so each client's watermark sees its revision.
  A stream whose rows reach `total`, at once for `total: 0`, answers its listing and returns the thread to the next call without waiting for the finish, which reaches only a subscribed connection: after a reconnect, a stream sent before the client's `state.bootstrap` is served receives every row and no finish, and waiting for one would cost the stream its deadline and the bootstrap queued behind it its budget.
  A finish that arrives after the answer is published like any other notification; the finish settles only a stream that falls short of `total`.
- The collector checks contiguity and the final count, and hands back a `MessageListing {account, mailbox, total, messages}`, the shape a `message.list` answer decodes into, so every caller keeps the `Vec` it builds today.
- `Queries` (`crates/mp-client/src/queries.rs:64`) gains `fn list_stream(&self, account: &str, mailbox: &str) -> Result<MessageListing>` with a default body that calls `message.list` with `limit: null` through `call` and decodes the answer into `MessageListing`; `Session` and `QueryHandle` override it with the session thread's streamed call.
  The default keeps the twelve other implementors working unchanged, production code among them: the desktop's `Budgeted` (`clients/desktop/src-tauri/src/session.rs:85`), and the TUI test fixtures in `src/tui_tests/queries.rs:250`, `actions.rs:1743` and `events.rs:1150`, which answer only `call` and through which `list_emails(&fixture, ...)` (`src/tui_tests/queries.rs:379`) lists a mailbox.
  A refusing default would break every one of them.
- `list_messages` (`queries.rs:109`) switches to it; `message_list_request` stays for the bounded callers.
- While it collects, the session thread serves no other call, exactly as it serves none while one large `message.list` answer is in flight today.
  A `state.resync_required` that arrives mid-stream is published at once, and the `state.bootstrap` it provokes waits behind the stream, so the finish of a stream short of `total`, a lifecycle event that survives an overflow, reaches the collector before any re-bootstrap can empty the queue.
- The budget of a plain call is `call_on`'s `recv_timeout` on the caller's thread (`crates/mp-client/src/session.rs:599`), and a call that outlives it runs to its end with its answer dropped (`call_within`, `:357`), so no component could cancel a stream from there.
  The streamed call therefore carries its deadline inside `Call`, and `serve` races that deadline while it collects.
  At the deadline `serve` sends `operation.cancel` for the stream's id, answers the caller with a timeout error, and keeps discarding that id's `message.rows` until its finish arrives, publishing every other notification as before.
  The caller waits on its channel for the deadline plus a short grace, so the answer it sees is `serve`'s and not its own `recv_timeout`.

### TUI

- `list_emails` (`clients/tui/src/queries.rs:64`) calls `q.list_stream` for every mailbox but Drafts, which stays on `draft.list`.
- `LoadMailbox` (`clients/tui/src/actions.rs:898`) keeps its shape: its thread still blocks on one call and posts one `Vec<EmailEntry>`.
  Its route in `clients/tui/src/commands.rs:137`, `ActionRoute::Daemon(&["message.list", "draft.list"])`, becomes `&["message.list_stream", "draft.list"]`, and the table that pins it in `src/tui_tests/actions.rs:318` changes with it.
- The staleness drop in `clients/tui/src/bg.rs:808` and `:1061` is unchanged, since the result still arrives whole; a stale stream runs to its end and is dropped.
- The re-issue after `apply_row_delta` answers `false` (`clients/tui/src/queries.rs:303`) goes through `list_emails` and therefore through the stream.
- The TUI's session requires `message.list_stream` at its handshake, so a TUI against an older daemon reaches the existing incompatible-daemon path instead of an empty mailbox.
  `client_session` (`src/daemon/client.rs:250`) passes no required capability today and serves the one-shot CLI commands too, so the requirement is added for the TUI's connector only.

### Desktop

- `list_messages_on` (`clients/desktop/src-tauri/src/commands.rs:701`) calls `list_stream` on its `Budgeted` (`commands.rs:707`) instead of `call` plus `decode_message_rows`, and still answers `MessageList::Messages {account, mailbox, total, rows}`, so the Tauri command `list_messages` and the TS side are unchanged in step 1.
- The TS reducer's `messages_loaded` (`clients/desktop/src/app/reducer.ts:1324`) keeps its atomic replacement of `state.messages`, and `overlayPending`, `filteredRows` and the generation guard see the same list they see today.
- `REQUIRED_CAPABILITIES` (`clients/desktop/src-tauri/src/connector.rs:88`) gains `message.list_stream`.
- The Rust layer's operation tracking does not register the stream's id in `pending` or `waiters` (`clients/desktop/src-tauri/src/session.rs:700` to `:780`): the collector in `mp-client` settles it.
  The pump's filter (`session.rs:839`) already drops an `operation.finished` for an id it does not await, with a debug line, so the frontend sees nothing new.
  `message.rows` never reaches the pump, because the session thread consumes it and `publish` drops a notification method it does not read.
- `Budgeted` overrides `list_stream` by matching its `Door` (`clients/desktop/src-tauri/src/session.rs:64`): `Door::Daemon` calls the `QueryHandle`'s streamed call with `Budgeted`'s budget as the deadline, and `Door::Fixture` takes the default body.
  The fixture door (`clients/desktop/src-tauri/src/fixture.rs:2939`) therefore keeps answering `message.list` from its existing listing, so fixture mode needs no stream simulation.

### Progressive rendering, a later step

Step 1 collects the stream and replaces the list whole, so no client behaviour changes.
A later ticket can render the first chunk before the last arrives: the desktop as a `messages_appended {key, gen, offset, rows}` action under the same generation guard, the TUI as a partial list behind its spinner.
That step also owes a cancel on staleness, where a mailbox switch sends `operation.cancel` for the stream it no longer wants, and a decision about how jump-to-date, the filters and select-all behave over a list that is still arriving.

## Tests

### Tests that pin the wire shape today, which must stay green

- `a_wire_row_is_the_json_literal_it_replaced` (`src/daemon/methods/message.rs:1926`), which holds a `WireRow` listing to the old `json!` literal byte for byte, now also the oracle for the rows a chunk carries.
- `tests/daemon_wire_rows.rs:217`, `:281` and `:345`, which decode `message.list` into `MessageListing`.
- `tests/daemon_protocol_fixtures.rs`, which holds `message.list.{request,response}.json` to their typed shapes.
- `crates/mp-protocol/tests/ts_bindings.rs:81`, the TS binding of `MessageListing`.
- `tests/daemon_framing.rs:618`, the `frame_too_large` mapping, and the `encode_capped` test in `src/daemon/server.rs`.
- `crates/mp-client/src/queries.rs` tests near `:547`, `src/tui_tests/{queries,actions,events,oracle}.rs`, and the desktop's `reducer.test.ts` and `tauri-mock.ts`.

### New tests

- Fixtures `crates/mp-protocol/fixtures/message.list_stream.request.json`, `message.list_stream.response.json` and `notification.message_rows.json`, in a list of their own in `tests/daemon_protocol_fixtures.rs` as `P5_U10C_FIXTURES` is one, each decoded into its typed shape.
- A test that the hand-built chunk frame equals `frame::encode` of the same notification as a `Value`, byte for byte.
- A daemon test over a real socket: a mailbox of a few thousand rows with `ROWS_CHUNK_BYTES` lowered for the test streams several chunks with contiguous offsets, every row equal to the `message.list` row for the same message, and the finish after the last chunk.
- Cancel mid-stream, with chunks still in the channel, settles `cancelled` with `-32008`, and no chunk follows the finish on the connection, because the writer drops the chunks of the shut token; a disconnect mid-stream cancels it; an unknown account or mailbox is refused at the call with no operation issued.
- An oversized single row fails the operation with `-32004` and the connection stays open.
- A `frame_too_large` regression at 50 000 rows that must now pass: the store seeded with 50 000 rows of about 500 bytes, `message.list` with `limit: null` still answers `-32004`, and `message.list_stream` through `mp-client`'s collector yields 50 000 rows with no frame over the cap.
  If seeding 50 000 rows inside a test proves too slow for the default run, it becomes an `#[ignore]`d test on `MP_BENCH_FIXTURE` beside the bench, and the default run keeps a 40 000-row variant.
- `mp-client` collector tests over a canned door: a gap in `offset`, rows past `total`, a short success and a failed finish each answer an error, and a deadline that passes mid-stream sends `operation.cancel`, answers the caller with a timeout and leaves the session serving the next call once the finish arrives.
- The default `Queries::list_stream` body over a door that answers only `call` yields the `message.list` listing.
- The desktop `REQUIRED_CAPABILITIES` test and the fixture door's panic on an unlisted method cover the new name.

### Bench

- `message_list_unbounded_bench` (`src/daemon/methods/message.rs:2040`) gains two steps on the same `MP_BENCH_FIXTURE=/var/tmp/mp-bench-50k` fixture: the store read plus every chunk encoded to bytes, and the client side, decoding the chunks into `MessageListRow`s.
- The run command stays the one in [message-list-unbounded.md](../baselines/message-list-unbounded.md).
- The expected daemon figure is about 41 ms for the read plus about 32 ms for the bytes, against 156 ms plus 22 ms of encode for `message.list` below the cap.

## Docs to update

- `docs/daemon-protocol.md`: "Transport and framing" (`:8`) for frames that are not answers and the 1 MiB chunk; "Method kinds" (`:111`) adds `message.list_stream` to the operations; the `message.list` section (`:281`) says what `limit: null` is for now; "Long-running operations" (`:865`) for an answer with members beside the id; "Error codes" (`:925`) for `frame_too_large` inside a finished event; "Delivery, coalescing and caps" (`:1087`) for row frames that bypass the outbound queue and the writer's order; "Notification methods" (`:1114`) goes from three to four; "Fixtures" (`:1126`); and a paragraph in the protocol changelog (`:1140`) ending in "the capability list grew by its name".
- [list-transfer.md](../baselines/decisions/list-transfer.md), updated in place: the whole-list decision holds, its transfer is now `message.list_stream` in 1 MiB chunks rather than one frame, the adopted method set names both methods, the "reopen past roughly 18 000 rows" condition gains the 50 000-row figure, and the P1a-U4 consequence that a listing is a large-payload path is answered by the stream.
- [message-list-unbounded.md](../baselines/message-list-unbounded.md): the "does not fit in a frame" section and the table gain the after-stream figures.
- `docs/daemon-operations.md:133` and `:150`, the TUI's listing paragraph and its `frame_too_large` note.
- `docs/parity-matrix.md:203`, `:212`, `:271` and `:915`, the TUI rows whose daemon surface is `message.list` for a mailbox open; `:261` and `:380` stay, since `mp list-messages` and `mp dump-mailbox` stay on `message.list`.
- `docs/lessons-learned.md:2456`, the entry on the `Value` tree, gains how the stream skips it.
- `clients/desktop/docs/rust-layer.md:83`, the `list_messages` command row.
- `CHANGELOG.md` `[Unreleased]`, and the `BACKLOG.md` line goes when this lands.
- No website page changes: no command or key binding moves.

## Rollout

1. Contract: the `mp-protocol` types and constant, the three fixtures, and the protocol document's sections, reviewed by Sylvain before any daemon code.
2. Daemon: the `ClientCtx` sink and the writer arm, then the method, its producer and the daemon tests.
3. `mp-client`: the session thread's streamed call with its deadline in `Call`, `Queries::list_stream` and its default body, `list_messages` switched, the collector tests.
4. TUI: `list_emails` switched, the `LoadMailbox` route in `clients/tui/src/commands.rs:137` and its pin in `src/tui_tests/actions.rs:318`, the handshake requirement, the TUI tests.
5. Desktop: `list_messages_on`, `Budgeted`'s `list_stream` over its `Door`, `REQUIRED_CAPABILITIES`.
6. The 50 000-row regression, the bench run, the baseline and decision documents, `CHANGELOG.md`, `BACKLOG.md`.

Each step lands on its own and leaves the tree green; until step 4 nothing calls the new method.

## Acceptance criteria

- [ ] A 50 000-row mailbox opens in the TUI and in the desktop client against a live daemon, where today it opens empty.
  Needs a live daemon and the Mac; `tests/daemon_list_stream_cap.rs` covers the path below the clients, 50 000 rows collected through `mp-client`'s session thread.
- [x] No frame of the stream exceeds `ROWS_CHUNK_BYTES` plus one row plus the envelope, checked in the regression test (`tests/daemon_list_stream_cap.rs`: 26 frames, the largest 1 049 188 bytes).
- [x] The streamed rows equal the `message.list` rows for the same mailbox, field for field and in the same order (`tests/daemon_list_stream.rs`, and the first 10 000 rows of the 50 000 in `tests/daemon_list_stream_cap.rs`).
- [ ] At 50 000 rows the daemon's read plus chunk encoding is under 80 ms on the baseline host, recorded in `message-list-unbounded.md`.
  Measured at 111 ms on the home server, where the store read alone is 89 ms against the baseline host's 41; the baseline host's figure is still to be taken.
- [x] `message.list` answers, fixtures and `mp list-messages` output are unchanged.
- [ ] A TUI or desktop build against a daemon without the method stops at the handshake with `capability_missing`.
  The TUI's requirement and its at-once refusal are unit-tested (`src/daemon/client.rs`, `src/daemon/session.rs`) and the desktop's list is pinned in `connector.rs`, which compiles only on the Mac; no run against an older daemon was made.
- [x] `cargo test --workspace` and the desktop's vitest run pass.
  The desktop crate's own `cargo test` is not part of the workspace and is owed on the Mac.

## Alternatives considered

### Offset paging

`message.list` with an `offset`, or a cursor, and a page size.
[list-transfer.md](../baselines/decisions/list-transfer.md) rejected an `offset` parameter for good, and nothing here overturns its reasons: jump-to-date, the filters and select-all run over the list the client holds, so a pager would still fetch every page and pay a round trip and a re-run query per page, `OFFSET` walks the skipped rows each time, and a mutation between two pages shifts the offsets into a duplicate or a hole unless the pages are pinned to one snapshot, which the stream gets for free from its single read.

### Raising the cap

A 32 MiB or 64 MiB `MAX_RESPONSE_BYTES` moves the ceiling to about 68 000 or 137 000 rows and removes nothing: the whole answer is still one frame held whole in the daemon's and the client's memory, the `Value` tree's 115 ms stays, and one frame holds the connection's events back for its whole write.
The cap also protects a client from a runaway answer, which a larger number weakens for every method at once.

### Compressing

A compressed answer needs either a binary framing or base64 inside a JSON line, and the protocol rules out base64 payloads in a frame.
It costs CPU on both ends, multiplies the ceiling by the compression ratio without removing it, and brings a new dependency.
The compact positional row encoding [list-transfer.md](../baselines/decisions/list-transfer.md) measured is the same trade with a smaller gain: 37% fewer bytes moves the ceiling to about 54 000 rows.

### Rows as `state.event` lifecycle events

The `message.server_hit` precedent, rejected under "The `message.rows` notification".

## Review points, as they landed

- The method is `message.list_stream` and the notification `message.rows`, as proposed.
- The TUI requires the capability at its handshake (`TUI_REQUIRED_CAPABILITIES` in `src/daemon/client.rs`), and a `capability_missing` there ends the run at once with `mp daemon restart` as the way out, instead of starting a daemon on demand that is already running.
- A `frame_too_large` from `message.list` with `limit: null` carries no `fallback`; it would need `encode_capped` to know the method.
- Every mailbox open is an operation: it takes a slot in the 256-operation memory, appears in a bootstrap snapshot's `operations` while it runs, and broadcasts a small `operation.finished` to every client.
  The TUI ignores the finish of an id it did not start without a log line (`a_mailbox_streams_own_finish_is_ignored` in `src/tui_tests/events.rs`) and renders no snapshot operations; the desktop pump drops it with a debug line, and the desktop reads the snapshot's operations only to find its own sign-in by id (`clients/desktop/src/app/signin.ts`).
- The calling connection must be subscribed to see the finish, as for every operation; the TUI and desktop sessions are, and the method does not refuse an unsubscribed caller.

## Follow-ups outside this ticket

- `mp list-messages -n 50000` and `mp dump-mailbox` on a mailbox that size still pass the cap on `message.list`; the envelope projection's record size at 50 000 rows is not measured.
- A `revision` on the immediate answer would let a client order the deltas it receives during a stream against the read; step 1 keeps today's race.
- The client still decodes every chunk through a `Value` (`mp_protocol::frame::Decoder` yields `Value`s); that cost is the client's and is not measured.
