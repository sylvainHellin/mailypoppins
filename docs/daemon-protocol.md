# Daemon protocol

The daemon owns the store, the account runtimes, and the sync engines, and every client reaches them over one JSON-RPC 2.0 connection.
This document is the contract for that connection: framing, the handshake, the method families, the error table, and the event semantics.
The Rust types live in `crates/mp-protocol`, and `crates/mp-protocol/fixtures/*.json` pins one committed example of every public shape.
A change to a field name, a numeric code, or the version range needs an entry in the protocol changelog at the end of this file.

## Transport and framing

A client opens a persistent full-duplex Unix-domain socket connection at `<data_dir>/runtime/daemon.sock` and keeps it for its lifetime.
The socket is created with mode 0600 inside a 0700 runtime directory, so the filesystem is the only access control the protocol relies on.

One message per line, UTF-8, terminated by a single `\n`.
A frame contains no raw newline: `serde_json` escapes newlines inside string values, so message bodies and subjects travel through line framing untouched.
`mp_protocol::frame::encode` produces exactly one frame, and `mp_protocol::frame::Decoder` reassembles frames from arbitrary read boundaries.

A request frame is capped at `MAX_REQUEST_BYTES`, which is 1 MiB.
The cap counts the terminator, so a frame of exactly 1 MiB is accepted and one byte more is refused.
It is enforced on the decoder's buffer rather than on a completed line, so a client that streams past the limit without ever sending a terminator is cut off instead of buffered.
The response direction carries its own cap, `MAX_RESPONSE_BYTES`, which is 16 MiB, which is why `Decoder::new` takes a limit instead of reading either constant.
Both sides read that one constant: the daemon refuses to write a reply above it and answers `frame_too_large` with `{limit, seen}` on the request's own id, and a client sizes its decoder by it, so an oversized answer is a named error on both ends rather than a truncated frame on one and a dropped connection on the other.

A breach of the cap, invalid UTF-8, or invalid JSON closes that connection and nothing else.
Other connections and the daemon itself keep running.

Large payloads do not travel as base64 inside a frame.
The daemon materialises attachments and other oversized results as files and returns a path with an explicit lifetime.

## Message shapes

A request carries `jsonrpc`, an optional `id`, a `method`, and an object `params`.
An absent `id` marks a request that expects no answer, and it is omitted from the JSON rather than sent as `null`.
`params` is always an object, never positional, and no method accepts SQL, a table name, or an internal store path.

A success response carries `jsonrpc`, the request's `id`, and a `result`.
An error response carries `jsonrpc`, the request's `id`, and an `error` of `{code, message, data}`, where `id` is absent only when the request could not be parsed far enough to read one.
`data` is omitted when the error code fixes no payload.

A notification carries `jsonrpc`, a `method`, and object `params`, and never an `id`.

A request id is a number or a string, and the daemon echoes back the value it received unchanged.

## Versions and the handshake

The protocol version is an integer, independent of the application version.
This build advertises the range `{min: 1, max: 1}`.

The first request on a connection is `initialize`, and it is the only method a client may call before initialization succeeds.
Its params are:

- `client.type`, one of `cli`, `tui`, or `gui`, and `client.version`, the client's application version. A fourth kind is `-32602`: a daemon that branches on the caller may not be handed a kind it has no branch for.
- `protocol.min` and `protocol.max`, the range the client can speak.
- `capabilities.required` and `capabilities.optional`, identifier lists.
- `identity.data_dir` and `identity.config_dir`, sent always and as a pair.

The identity pair is unconditional because `MAILYPOPPINS_DATA_DIR` and `MAILYPOPPINS_CONFIG_DIR` are independent.
A client with only the config override set would otherwise reach a daemon holding different config, secrets, and signatures.

The result names:

- `daemon.version`, the daemon's application version.
- `protocol.selected`, the version both sides will speak, always inside the daemon's range.
- `instance_id`, which identifies this daemon process and appears in every event.
- `capabilities`, the identifiers the daemon offers, in the daemon's own order.
- `platform`, the host and transport facts a client cannot infer: `os` as Rust names it (`linux`, `macos`) and `transport`, which is `unix_socket` on every connection this build serves.
- `lifecycle`, the shutdown and restart behaviour of this instance: `idle_shutdown_seconds`, `restart_required`, `shutdown_on_last_client`. A daemon that runs until it is stopped reports a null idle timeout and both flags false.
- `config_status`, the current state of the configuration on disk: `state` of `ok`, `absent` or `invalid`, the `path` the daemon read, the `accounts` count, and `problems`, whose first entry is the message a client shows the user when the state is `invalid`.

A daemon serves in all three configuration states.
An absent or unparseable `config.toml` is zero accounts and a diagnostic, never a refused connection or a refused startup.

Compatibility is decided by the declared ranges and the required capabilities.
Disjoint ranges give `protocol_incompatible`, a required capability the daemon does not offer gives `capability_missing`, and a differing directory pair gives `identity_mismatch`.
The directory pair is compared canonically, so a symlinked path is not a mismatch, and the refusal reports the paths as each side spelled them.
An application-version difference alone is diagnostic and does not refuse the connection.

A capability identifier names a method family or a behaviour the daemon will serve, and a client requires only what it cannot work without.
The list a build advertises is exactly the methods it serves: the two lifecycle methods followed by every method registered on the dispatcher, in method-name order, derived at handshake time rather than written out, so a method cannot be served without being advertised or advertised without being served.
A daemon started with the `test.operation` hook of `docs/daemon-operations.md` advertises that method too, which is the derivation working rather than an exception to it.
Requiring one this build does not have is a `capability_missing` at the handshake rather than a `-32601` at the first call, and an *optional* capability the daemon lacks is dropped from the connection's agreed set instead of refusing it.

The handshake happens once per connection, and a second `initialize` on the same connection is `-32600`.
The session state dies with the connection: it is never expired, reused, or transferred.

Any method other than `initialize` and the two lifecycle methods below issued before a successful `initialize` gives `not_initialized`, ahead of the method lookup, so an uninitialized client cannot probe which methods a daemon serves.
The refusal is per request rather than per connection: the connection stays usable and the `initialize` that should have come first still works on it.

## Method families

Methods are domain operations, and each one declares whether it is a query, a command, a long-running operation, or a client-side integration request.
The families, all of them reserved here and served over the phases of the migration:

- `state.*` for bootstrap and state diagnostics.
- `account.*` for listing, selection metadata, sync health, and account operations.
- `mailbox.*` for listings, counts, and mailbox metadata.
- `message.*` for listing, retrieval, search, selection, mutation, attachments, and browser materialisation.
- `draft.*` for creation, parsing status, validation, recipient editing, reply, reply-all, forward, approval, discard, and attachment changes. This build serves the ten methods of the draft slice and the mutation slice.
- `send.*` for immediate send, approved batches, invitations, outbox recovery, and, from Phase 6, hold countdowns and their cancellation. This build serves the six methods of the send slice: `send.approved`, `send.draft`, `send.invite`, `send.outbox_discard`, `send.outbox_list` and `send.outbox_retry`, whose result types are `mp_protocol::send`.
- `sync.*` for quick sync, full sync, progress, and errors.
- `contact.*` for listing, ranking, rebuilding, and statistics. This build serves `contact.rebuild`, `contact.search` and `contact.stats`, whose rows are `{address, display_name, sent_to, sent_cc, received, score}`.
- `calendar.*` for agenda queries, invitations, RSVP, updates, and cancellations. This build serves `calendar.rebuild` and `calendar.rsvp`.
- `signature.*` for list, read, create, update, rename, delete, and per-account default selection.
- `config.*` for safe reads, validation, updates, reload, account setup, authentication, and secret writes. This build serves nine methods: `config.add_account`, `config.cutover`, `config.get`, `config.init`, `config.oauth2_login`, `config.reload`, `config.reset_secrets`, `config.set_password` and `config.validate`.
- `operation.*` for long-running operation status and cancellation.
- `diagnostic.*` for logs, health, and support information. This build serves `diagnostic.store_gc`, the retention sweep `mp store gc` runs; there is no `store.*` family, so the sweep is served here, which is the name `docs/parity-matrix.md` SYN-08 already carried.
- `daemon.*` for status and graceful lifecycle control.

`initialize` is the one method outside a family, because it runs before any family gate exists.

### Method kinds

Every method registered on the dispatcher declares a kind, and the kind fixes what its answer carries beyond `result`: a `revision`, which is the daemon state revision the call moved to, and `affected`, the resources whose cached copies the call invalidated (`account:work`, `mailbox:work/inbox`, `message:work/inbox/41`).
Both are daemon-side facts and do not appear in the JSON-RPC `result`; they are what the daemon fans out as `state.event` notifications, so a client that applied an event never has to guess which of its caches went stale.

- **Query** reads and changes nothing, so its answer carries no revision and no affected resource. `account.list`, `mailbox.list`, `mailbox.list_server`, `message.get`, `message.list`, `message.list_server`, `message.search`, `message.release_handle`, `operation.status`, `state.bootstrap`, `draft.list`, `draft.path`, `draft.preview`, `draft.validate`, `send.outbox_list`, `contact.search`, `contact.stats`, `config.get` and `config.validate` are the queries this build serves. The two `*.list_server` queries open a session on the account's mail server rather than reading the store, and are queries all the same: they write nothing, here or there.
- **Command** changes state at once, so its answer carries the revision the change moved the daemon to and at least one affected resource. A command that changed nothing observable is a query, and a command with an empty `affected` would leave every client stale with no event to fix it. `operation.cancel`, `config.reload`, `config.set_password`, `config.add_account`, `config.init`, `config.reset_secrets`, `message.archive`, `message.delete`, `send.outbox_discard` and the six `draft.*` writers are the commands this build serves; a reload that reconciled nothing is the one case with an empty `affected`, and it still announces itself with a `config.changed` event.
- **Operation** runs long enough to be worth cancelling and observes a cancellation token. `sync.quick`, `sync.full`, `sync.watch`, `send.approved`, `send.draft`, `send.invite`, `send.outbox_retry`, `contact.rebuild`, `calendar.rebuild`, `calendar.rsvp`, `diagnostic.store_gc`, `config.cutover` and `config.oauth2_login` are the operations this build serves, and the `test.operation` hook registers one more. Cancelling is the method's own answer, `operation_cancelled` (`-32008`) with `{operation_id}`, never a cancellation imposed on it from outside: a method that has already committed a write reports the write rather than being reported as cancelled behind its own back.
- **ClientIntegration** is work only the client's process can do, such as opening a browser or revealing a file. The daemon answers with the instruction and the client carries it out.

A method also declares `since`, the first protocol version that served it, which is never below `1`, and `cancel_scope`, one of `durable` or `client_scoped`, which says what a disconnect of the calling connection does to the work the call started.
`durable` is the default and what a method that never thought about cancellation means: the work outlives the client that asked for it.
A method declares `client_scoped` when its work has no reason to continue once the client that wanted the answer is gone.

`initialize`, `daemon.status` and `daemon.stop` declare no kind, because they are not dispatcher methods: they are lifecycle surface answered by the connection itself, ahead of the handshake gate and outside the domain.

### Lifecycle methods

`daemon.status` and `daemon.stop` are the two methods reachable **before** the handshake.
They are lifecycle surface rather than domain surface, they touch no account data, and the exemption is what lets `mp daemon status` describe a daemon whose protocol range it cannot negotiate and `mp daemon stop` end one.
Every other method, known or unknown, is gated.

`daemon.status` takes `{}` and returns:

```json
{
  "instance_id": "1f0c…",
  "app_version": "0.9.0",
  "protocol": {"min": 1, "max": 1},
  "pid": 40321,
  "started_at": "2026-01-01T09:12:44.512Z",
  "data_dir": "/home/alice/.local/share/mailypoppins",
  "config_dir": "/home/alice/.config/mailypoppins",
  "accounts": [{"name": "work", "state": "opening"}]
}
```

The fields are the daemon's own `daemon.json` metadata plus the live account list, so a client comparing them against its own paths learns whether it is talking to the daemon it meant to.
`accounts` is empty unless the daemon was started with `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES=1`, and an account state is one of `opening`, `ready`, `blocked`: `opening` while its runtime is still starting, then whichever the runtime reported.
The shape does not change with the opt-in, only which states appear in it.
`mp daemon status --json` prints this object with a leading `"running": true`, or the same keys with null values and `"running": false` when nothing answers.

`daemon.stop` takes `{}` and returns `{"stopping": true}`.
The response is written and flushed before the shutdown starts, so the caller always learns the daemon accepted the request.
Open connections are not drained: the daemon unlinks its runtime files and exits, and a client that loses the socket mid-call reconnects.

### Bootstrap

`state.bootstrap` takes `{}` and hands over the whole state a client mirrors, plus the revision it was captured at, as one serialised operation:

```json
{
  "instance_id": "1f0c…",
  "revision": 4217,
  "capabilities": ["state.bootstrap", "account.list"],
  "snapshot": {
    "accounts": [{"name": "work", "state": "opening", "sync_health": {"state": "unknown"}}],
    "mailboxes": {"work": [{"role": "inbox", "slug": "inbox", "label": "Inbox", "total": 0, "unread": 0, "badge": 0}]},
    "drafts": {"work": [{"id": "d-one", "path": "/home/alice/.local/share/mailypoppins/accounts/work/drafts/angebot.md", "to": "robin@example.com", "subject": "Angebot", "status": "draft", "valid": true, "ready": true}]},
    "outbox": {"work": {"queued": 0, "failed": 0}},
    "holds": [],
    "operations": [],
    "diagnostics": []
  }
}
```

`capabilities` is what **this connection** agreed on at its handshake, not the daemon's whole list: a client acts on what it may use, and the offer is already in the `initialize` result.
`revision` is never `0`, which the client keeps as its own pre-bootstrap sentinel.

`mailboxes`, `drafts` and `outbox` carry one key per listed account, always, so a client indexes them by account name without a null check.
A daemon with no configured account answers an empty `accounts` array and three empty objects.
`holds` and `diagnostics` are arrays that nothing in this build fills.
`operations` lists every long-running operation the daemon has not settled, in start order, each entry being an `operation.status` result, so a client that bootstraps while work is in flight learns about it without having been there when it started.

A draft row carries `{id, path, to, subject, status, valid, ready}`, the same fields the `draft.changed` event carries, so the reducer a client writes is "replace the row with the payload" rather than a projection it has to keep in step.
`id` is the `id:` frontmatter field, or the file stem when the file has none; `path` is absolute, because a GUI opens a draft by path; `to` is `null` for a draft with no recipient yet.
`valid` is "the file parsed" and `ready` is "it would send", which are two axes: a draft with no subject parses perfectly and is not sendable.
A draft that does not parse stays a row with `valid: false`, `status: "invalid"`, `to: null`, `subject: ""` and `ready: false` (#0080): `"invalid"` is not a draft status a file can spell, and that is the point, since nothing read a status out of a file that would not parse.

An account's `state` is one of `opening`, `ready` or `blocked`, and it reports the runtime rather than the store: it answers "has this account's runtime come up", where `account.list`'s `state` answers "can I read this account's store on disk".
The two are deliberately different questions.
Without `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES=1` no runtime is ever started, so every account stays `opening`; with it, an account is `opening` in a snapshot taken before its runtime came back and converges by event afterwards.
For an `opening` account all counts are `0` and the draft list is empty, exactly as the TUI presents an account it has not opened yet; readiness arrives afterwards as an ordinary event.
`sync_health` is an object whose `state` is `unknown`, `ok` or `failed`, and a fresh bootstrap reports `unknown`.

The ordering rule is the reason a bootstrap is one serialised operation.
The daemon registers the connection as a subscriber **before** it captures the snapshot, so every change committed from the registration onwards is already queued; a register-last daemon loses exactly the changes that land between the capture and the start of queuing.
The client initialises its watermark to the reported revision and silently drops every event at or below it, which is what makes register-first safe: an event for a change the snapshot already carries is recognised as redundant instead of applied twice.
The drop is silent by design, because a correct daemon queues that event in the first place and a `duplicate` marker on the wire would be a shape carried for nothing.
Together the two rules give the invariant: a change made anywhere around a bootstrap reaches the client either as one delivered event or as part of the snapshot, never as two applications and never as none.

`state.bootstrap` is a query and sits behind the handshake gate like every other domain method: called before `initialize` it is `not_initialized`, and the connection stays usable.
Bootstrapping twice on one connection is allowed and is what a client does after a gap, a resync request or an instance change.

### Read-only methods

`account.list`, `mailbox.list`, `message.get`, `message.list` and `message.search` are the read-only domain methods.
All of them take the store path the CLI takes (`store::read`) and none acquires the account's `EngineLock`: the daemon does not become an account's engine before Phase 5, so a running TUI or `mp sync` keeps the lock while the daemon answers reads beside it.
No read result names a file: a client that cannot open the store must not be handed a path into it, which is the dump's own contract (`docs/dump-allow-list.md`) applied to every one of them.

`account.list` takes `{}` and returns:

```json
{"accounts": [{"name": "work", "default": true, "backend": "imap", "state": "ready"}]}
```

It reports the configuration, not the runtimes: every `[[accounts]]` entry of `config.toml`, in the file's order, which is the order an `-A`-less command already treats as authoritative.
`default` is true for the first configured account and no other, because the CLI has no other notion of a default.
`backend` follows `auth_method` alone, `graph` for Graph and `imap` for everything else, and is independent of `state`.
`state` is decided on disk: an account whose store exists and opens is `ready`, and one with no store yet is `blocked`, since it cannot serve a read until `mp sync` writes one.
This build never reports `opening` here: nothing on this path is asynchronous, so no account is ever between states, which is why the same account is `opening` in a bootstrap snapshot and `ready` or `blocked` in this answer.
The two report different things, the runtime and the store on disk, and the bootstrap section above says which is which.

That `state` is probed read-only: the daemon opens the store file with `SQLITE_OPEN_READ_ONLY` and checks the schema stamp and the required tables, rather than going through `Store::open`, which creates a missing store and rebuilds a corrupt one.
Asking which accounts exist may not create or destroy a cache, so a file that fails the probe is `blocked` and is left exactly as it was found.

`mailbox.list` takes `{"account": str}` and returns one account's sidebar hierarchy:

```json
{
  "account": "work",
  "mailboxes": [
    {"role": "inbox", "slug": "inbox", "label": "Inbox", "total": 214, "unread": 7, "badge": 214},
    {"role": "drafts", "slug": "drafts", "label": "Drafts", "total": 2, "unread": 0, "badge": 2}
  ]
}
```

The order, the roles, the slugs and the labels are the bootstrap snapshot's `mailboxes` entry for that account, from the one hierarchy the TUI sidebar also builds, so two answers about one account cannot disagree about which mailboxes it has.
`role` is one of `inbox`, `drafts`, `sent`, `archive`, `other`; `slug` is the store key a client addresses in `message.list`; `label` is what a sidebar prints.
Drafts are listed here and refused by `message.list`, exactly as the sidebar lists them and the message listing does not.
`total` is every message the mailbox holds, the count the sidebar shows, with the Drafts row counted from the draft index because drafts are not message rows.
`unread` is how many of them the server has not flagged `\Seen`, and is `0` for Drafts, which have no read state.
`badge` is what the sidebar prints beside the label, which is `total` in this build.
Unlike the other two, `mailbox.list` opens the account's store twice, once for the grouped totals and once to count the rows without `\Seen`; it is a read either way and the second open is a cost rather than a contract, recorded in ticket #0121.
An unknown account is `account_unknown` and a configured account with no readable store is `account_not_ready`, the same two refusals `message.list` makes.

`message.list` takes `{"account": str, "mailbox": str, "limit": u32|null}` and returns:

```json
{
  "account": "work",
  "mailbox": "inbox",
  "total": 2,
  "messages": [{
    "uid": 1,
    "message_id": "<Bericht@example.com>",
    "from": "Ivana <ivana@example.com>",
    "subject": "Bericht",
    "date_sort": "2026-07-02T11:57:30",
    "date_display": "Thu, 2 Jul 2026 13:57:30 +0200",
    "flags": {"seen": true, "answered": true, "forwarded": true},
    "has_attachments": true
  }]
}
```

`mailbox` accepts a role, a slug or a sidebar label, exactly as `mp list-messages --mailbox` does, and the answer echoes the resolved id rather than the spelling that was sent.
The order is the store's, `date_sort DESC, id DESC`, the same rows `mp list-messages` and the TUI list show.
`message_id` is verbatim as ingest stored it, angle brackets included; `from`, `subject` and `date_display` travel as `""` when the header was absent, because the shape says `str`.
The two dates are both carried because neither can be derived from the other: `date_sort` is `tui::app::resolve_date`'s UTC sort key, so every stack orders the same way, and `date_display` is the `Date:` header as the store holds it, which is the column a listing prints.
`flags` carries the three axes named above and not the store's fourth, `\Flagged`.
`total` is how many messages the mailbox holds and ignores `limit`: it is the "In the store: N" of `mp list-messages`.
An absent `limit` and `limit: null` both mean every message; `limit: 0` means none, since `null` already spells "all" and a number may not mean the opposite of itself.
An empty mailbox of a ready account is an empty listing, not an error.

`mp list-messages` renders from the wire alone: it opens no store of its own, and a client that never had one prints the same listing.

`projection` selects what a listing carries. It is `"list"` by default, which is the shape above; anything other than `"list"` or `"envelope"` is `-32602`.

`projection: "envelope"` takes `{"account": str, "mailbox": str|[str]|null}` and returns the envelope records `mp dump-mailbox --json` prints:

```json
{
  "account": "work",
  "records": [{
    "account": "work",
    "mailbox": "inbox",
    "message_id": "<Bericht@example.com>",
    "from": "Ivana <ivana@example.com>",
    "to": "alice@example.com",
    "cc": null,
    "subject": "Bericht",
    "date_sort": "2026-07-02T11:57:30",
    "flags": ["answered", "seen"],
    "attachments": [{"name": "notes.pdf", "size": 14}],
    "invite": false,
    "thread": "<Bericht@example.com>"
  }]
}
```

The records are `dump::EnvelopeRecord`, so the client re-serialises them with `dump::to_ndjson` and the NDJSON ordering contract, the field order and the null handling stay in the one module that already owns them.
The projection is per account and covers every selected mailbox in one answer, because the dump's sort key is `(account, mailbox, date_sort, message_id, subject, uid)` and a client that merged per-mailbox answers would be re-implementing it; the client's only ordering duty is to call the accounts in ascending name order.
`mailbox` accepts a name, an array of names (the repeatable `--mailbox`) or `null`/absent for every listable mailbox, matched against the mailbox id and its sidebar label case-insensitively.
A name that is not one of the account's mailboxes selects nothing rather than failing, exactly as `dump::collect_records` treats an unmatched filter: a filter narrows a dump, it does not assert anything about it.
A storeless account is `account_not_ready` here as everywhere else, and the *client* turns that into "no records" for the dump, because `collect_records` skips an account it cannot open rather than refusing the run.

`message.get` takes `{"account": str, "id": str}` or `{"account": str, "selector": str, "mailbox": str|null}`, plus `body: bool`, and returns the record `mp show --json` prints (`read_cmd::ShownMessage`):

```json
{
  "selector": "mp://work/inbox/Bericht@example.com",
  "account": "work",
  "mailbox": "inbox",
  "message_id": "<Bericht@example.com>",
  "from": "Ivana <ivana@example.com>",
  "to": "alice@example.com",
  "cc": null,
  "subject": "Bericht",
  "date": "Thu, 2 Jul 2026 13:57:30 +0200",
  "flags": ["read", "answered"],
  "invite": false,
  "attachments": [{"name": "notes.pdf", "size": 14}],
  "body": "der quarterly ledger ist beigefügt\n"
}
```

The result *is* that record, field for field, and the text answer is `read_cmd::render_show` over the same one, so a client that received the payload prints either answer without opening a store and the JSON answer is the payload re-serialised rather than a second projection that could drift from it.
HTML-only mail reads as the flattened text ingest derived, never as markup.

A message is addressed either by `id` or by `selector`, never by both and never by neither; both mistakes are `-32602`.
`id` is the `"<mailbox>/<uid>"` of the handle methods, taken literally, because a GUI holding a listed row has no selector to spell.
`selector` is the grammar `mp show` takes from a user (`<message-id>`, `<mailbox>/<message-id>`, `mp://<account>/<mailbox>/<message-id>`), resolved daemon-side the way `resolve_received_arg` resolves it, with the optional `mailbox` narrowing it exactly as `mp show --mailbox` does: the resolution needs the store the client no longer has.
Which *account* a selector names stays a client-side decision, because `Selector::parse` needs no store.
An unknown uid, a malformed id, a mailbox the account does not have and a selector that resolves to nothing are all `-32602`, in the resolution's own words, so a routed `mp show` reports what the pre-daemon one reported.

`body` defaults to `true` and its absence is not its nullity: with `body: false` the key is gone from the result, and with the default it is present and `null` when the store holds no readable body for the row.
`mp show` prints its "no stored body" sentence for exactly that `null`, so collapsing the two would make a bodyless answer indistinguishable from an evicted blob.

`message.search` takes `{"account": str, "query": str}` plus `mailbox`, `limit`, `body`, and the field filters `from`, `to`, `cc`, `subject`, `body_query`, `filename`, `has_attachment`, `after`, `before`, and returns:

```json
{
  "account": "work",
  "query": "ledger",
  "hits": [{
    "uid": 3,
    "mailbox": "inbox",
    "message_id": "<markup@example.com>",
    "from": "designer@example.com",
    "subject": "Ledger in HTML",
    "date_sort": "2026-06-30T06:15:00",
    "date_display": "Tue, 30 Jun 2026 08:15:00 +0200",
    "flags": {"seen": true, "answered": false, "forwarded": false},
    "has_attachments": false
  }]
}
```

The params mirror `mp search --local`'s flags and the daemon builds the query with `search::from_cli`, so one parser serves every backend and the client sends what the user typed.
`body_query` is the wire name of the `--body` flag, because `body` is already the "send me the bodies" switch (`--full`) and one key may not mean two things; `body` defaults to `false` here and to `true` in `message.get`, deliberately, since `mp show` always prints a body and `mp search` prints one only under `--full`.
Hits come back in the store's ranking order, which is what `mp search --local` prints under "best match first", and each hit is a `message.list` row plus the `mailbox` it was found in, which is what `Selector::for_message` needs to render the line.
`mailbox` is resolved as `message.list` resolves it and falls back to the query's own `in:` directive; an unknown one is `-32602`.
An absent `limit` means every hit.
A query the search layer cannot use, a lone quote or bare punctuation, is `-32602` and not `-32603`: the parameter is wrong, not the store.
A query nothing matches is an empty `hits` array, not an error.

### Message mutations

Two methods change a received message, and each of them owes the server an operation.

| method | kind | params | result |
|---|---|---|---|
| `message.archive` | command | `{account, id\|selector, mailbox?}` | `{account, id, selector, mailbox, moved_to: {mailbox, selector}}` |
| `message.delete` | command | `{account, id\|selector, mailbox?}` | `{account, id, selector, mailbox}` |

Two methods rather than two flavours of one: archiving moves a row and owes a `Move`, deleting drops a row and owes a `Delete`, and only the first has anything to roll back when the server refuses.
Both are commands, because each moves the daemon's revision and invalidates `message:<account>/<mailbox>/<uid>`, and both are `durable`: the daemon commits the row change and the owed server op in one transaction and then drains that op synchronously (#0039), and a drain torn down because the calling socket went away would leave the op queued while its caller was told nothing.
The answer is therefore the settled outcome rather than an acknowledgement, which is what preserves the blocking UX `mp archive` and `mp delete` have always had.

**A message is addressed exactly as `message.get` addresses one**: `id` is `"<mailbox>/<uid>"`, `selector` is the grammar the user types, `mailbox` narrows a selector the way `--mailbox` does, and neither or both is `-32602`.
The refusals are that resolution's own sentences, ambiguity included, so a routed command reports what the pre-daemon one reported.

**The backend is resolved before the store is touched.**
An account whose credentials cannot be loaded refuses with the secret store's own sentence and leaves the row exactly where it was, rather than archiving it locally and queueing a move behind a password nobody has entered.
That refusal is `-32603` with `{account}`: the account is configured and its store is readable, so `-32005` and `-32006` would contradict what `account.list` says about the same account, and the caller's parameters were right, so it is not `-32602`.
Protocol 1 has no credentials code, and adding one would take a changelog entry without moving a user-visible byte.
The secrets backend is opened on first use rather than at startup, the same rule `config.set_password` follows.

`moved_to.selector` names the Message-ID as the store holds it, which is the "now" line `mp archive` prints.

### Materialised handles

A client cannot read an account's blob store, so the daemon writes the bytes it asks for into its own runtime directory and hands back a path with an explicit lifetime.

| method | kind | params | result |
|---|---|---|---|
| `message.materialise_attachment` | client_integration | `{account, id\|selector, mailbox?, part}` | `{handle, path, name, bytes, expires_at}` |
| `message.materialise_html` | client_integration | `{account, id\|selector, mailbox?}` | `{handle, path, name, bytes, expires_at}` |
| `message.release_handle` | query | `{handle}` | `{}` |

Like the read-only methods, all three open the store by path and take no engine lock: materialising is a read plus a write into the daemon's own directory, and neither makes the daemon an account's engine.
The two materialisers are *client_integration* because the daemon prepares the file and only the client's own process can open it; the release is a *query* because it moves no revision and invalidates no resource, handles being per-client scratch that appears in no snapshot and in no event.

**A message is addressed as `"<mailbox>/<uid>"`**, the last two thirds of the `message:work/inbox/41` resource, which a client composes from the mailbox it listed and the `uid` of the row it is holding.
The mailbox is taken literally rather than resolved through the account's configured roles, so a message in a mailbox the configuration no longer lists is still materialisable: a handle is about bytes in the store, not about what the sidebar shows.
The store's row id was the other candidate and is a rebuild away from meaning a different message; the `Message-ID` header was the third and is shared by the Inbox and the Sent copy of one message.

**`{selector, mailbox?}` addresses one too**, added in P4-U8 beside the `{id}` form and resolved exactly as `message.get` resolves it.
A client holding a selector cannot build `"<mailbox>/<uid>"` out of a `message.get` record, which carries the mailbox and the `Message-ID` rather than the uid, and re-listing the mailbox to find that uid would be a second query to answer a question the daemon already answers.

**`name` is the sanitised file name**, so a client builds its own destination without parsing the daemon's path.
The daemon never renames a part: two parts sent under one name come back as two handles carrying that one name, in two directories, and the `_1` rule that turns them into two files belongs where the names become paths, which is the client (`mp save` applies it within one call, so saving the same message twice writes the same two names rather than growing a copy per run).
There is no `mime` on the wire: the store keeps no content type for a part, and deriving one from the extension is a guess a client can make for itself.

**`part` is a dense zero-based index into the message's user-facing attachment list**, the order `mp save` and the TUI show, with the iMIP sidecar excluded.
It is the index of the row a client is looking at; addressing by the store's raw `ordinal` would leak the hidden sidecar's position into a list it is deliberately absent from.

**`message.materialise_html` writes the browser rendition, not the raw markup**: the charset and the `Content-Security-Policy` meta tag the TUI's `b` binding injects before it hands a `file://` URL to a browser (#0037), with `cid:` references inlined as `data:` URIs.
A sender who wrote no markup is `-32602`, not a daemon failure.

**The file lands at `<data_dir>/runtime/handles/<handle>/<name>`**, one directory per handle at mode 0700, where `<name>` is the sanitised attachment filename or `message.html`.
One directory per handle is what lets the file keep the sender's own name (`vertrag.pdf`, not a hash) without two handles colliding, and what makes a release a directory removal derivable from the id alone.
The name is sanitised with the rule `mp save` applies, so a hostile `Content-Disposition` cannot escape the directory.
`bytes` is the length of the file at `path` and not the size of the backing blob: for an attachment the two agree, for a rendition they do not, because the CSP tag is added after the blob is read.
`handle` is opaque, non-empty and drawn from `[A-Za-z0-9_-]`, because it is also a directory name; a client echoes it and never parses it.

**Expiry, not disconnection, ends a handle.**
All three methods are `durable`: a GUI that opened an attachment in a viewer and then lost its connection must not have the file pulled out from under it.
`expires_at` is RFC 3339, one lifetime ahead of the call, and a handle is live strictly before that instant.
The lifetime is ten minutes, overridable through `MAILYPOPPINS_DAEMON_HANDLE_TTL_MS` ([daemon-operations.md](daemon-operations.md#materialised-handles)).
Expired handles are reaped lazily, at the top of every handle call: the entry goes and its directory is unlinked.
A client that needs its file longer materialises it again rather than betting on a comparison.

**A live handle pins every blob its materialisation read**, and the retention sweep skips a pinned blob (`ANO-6`), so a sweep running beside a viewer cannot evict the file it has open.
An attachment pins one blob; a rendition pins the `html` blob, plus the `raw` blob when the markup carried `cid:` references and the inline-image scan had to parse it.
The pin ends with the release or with the expiry, and the next sweep reclaims.
It holds back candidates and nothing else: the store's size, the warn-then-evict marker and the half-store guard are unchanged, and `--force` overrules that guard and never a pin.

An unknown account is `-32005` with `{account}` and a configured account with no readable store is `-32006`, the two refusals every read method makes.
Everything else a caller can get wrong is `-32602`: an id that is not `"<mailbox>/<uid>"`, a message the account does not hold, a `part` that is not one of the message's attachments or is absent, a message with no markup, and an unknown, already released or expired handle.
Those last three are one answer on purpose: all of them mean "you are not holding that", and which of the three it was is not a distinction a client can act on.

### The `config.*` family

The daemon owns the configuration: it is the only component that parses a complete `config.toml`, and `config.*` is how a client reads it, checks an edit, swaps it, adds an account, stores a password, logs in, resets the secrets and migrates a file-era account.
Nine methods, all of them `durable`, because a configuration swap undone by a disconnect would leave the daemon serving a configuration nobody chose.

| method | kind | params | result |
|---|---|---|---|
| `config.add_account` | command | `{account: {…}}` | `{added, updated, removed}` |
| `config.cutover` | operation | `{account, dry_run}` | `{account, dry_run, drafts: {imported: [path], already_indexed, skipped: [line], collisions: [line]}, remnants: [{path, md_files, bytes}]}` |
| `config.get` | query | `{}` | `{revision, path, state, config}` |
| `config.init` | command | `{account: {…}, secrets_backend?, theme?, notifications?}` | `{added, updated, removed, path}` |
| `config.oauth2_login` | operation | `{account}` | `{stored, account, kind, key}` |
| `config.reload` | command | `{}` | `{added, updated, removed}` |
| `config.reset_secrets` | command | `{}` | `{removed: [path]}` |
| `config.set_password` | command | `{account, kind, value}` | `{stored, account, kind, key}` |
| `config.validate` | query | `{toml}` | `{ok}` or `{ok: false, errors: [{line, message}]}` |

`config.oauth2_login` is the one method whose progress a user has to read: it publishes one `operation.progress` with `phase: "device_code"` and `message: "<url> <code>"`, which is the verification URL and the code, and the client renders the block from it. Nothing else about the login travels, the access token least of all.

`config.cutover`'s `skipped` and `collisions` are rendered lines rather than structured rows, because both are `Display` types the CLI prints verbatim and a client that re-worded them would be inventing a second spelling of one fact.

`config.reset_secrets` removes the encrypted secrets file first and then every OAuth2 token cache in path order, which is the order it lists them in `removed`.

**The configuration revision.** `config.get` wraps the configuration rather than being it, because a client that reads one needs to know *which* one it read.
`revision` starts at 0 for whatever the daemon loaded at startup, including "no configuration at all", and moves by one per successful swap; it is the same counter `config.changed` carries as `config_revision`, and it is not the state revision, which moves on every event from every source.
A rejected candidate moves it not at all, because nothing was swapped, which is why `config.invalid` carries no revision.
`path` and `state` are the two facts `initialize`'s `config_status` reports, spelled the same way (`ok`, `absent`, `invalid`), recomputed from the live snapshot so a client that connected before a `config.init` does not have to reconnect to learn the file now exists.

**Effective means after serde defaults, not after the engine's clamps.**
A `config.toml` that omits `smtp.port` reports `465`, because that is what `GlobalConfig` holds once it has loaded.
The `[1, 8]` and `[0, 600]` clamps on `imap.fetch_concurrency` and `imap.body_fetch_deadline_secs` belong to `ImapConfig::load`, which needs credentials and is not on this path, so `config.get` reports the loaded value.
Retention is reported resolved, per-account overrides layered over the global table with every optional filled in, which is what the engine acts on.

**Secrets never travel.** `smtp.password` and `imap.password` are always present and always the literal `<redacted>`.
Present, because their absence would say "no password is stored", which is a fact about the secrets backend and one a redacted read must not go and look up; the literal, because a length, a prefix or a fixed number of asterisks all leak something.
`oauth2.client_id` is not redacted: it is a public identifier, `mp config show` prints it in the clear, and redacting it would break the one screen that exists to diagnose an OAuth2 setup.

**`config.validate` is a pure function of the string it is given.**
It reads no file, writes no file, swaps nothing and moves no revision: it is the "would this load?" a GUI asks while the user is still typing, running the same three checks the loader runs (legacy keys, the TOML parse, retention) against the parameter.
A `line` is the 1-based line of the offending token, and `null` when the diagnostic has no position: a TOML syntax error carries a span, a semantic refusal does not, and inventing a position would send a user to an innocent line.

**A swap is stop-removed, then update, then start-added, and the announcement comes last.**
A removed account publishes `state.remove` of `account:<name>`; an updated or added one publishes `account.state_changed` when its runtime settles; `config.changed` is published after every per-account event of that swap, so everything the reload did is in front of the event that announces it.
The order matters beyond tidiness: releasing a removed account's engine lock before an added account tries to take one is what lets an account be renamed in one edit without the new runtime losing a race to the old one.
`config.reload` answers only once every runtime it touched has settled, so "the lock is free again" is not something a caller has to poll for.
An account counts as *updated* when the object `config.get` reports for it changed; the three lists are sorted lexicographically, so two daemons reconciling the same edit report it identically.
Every successful swap publishes exactly one `config.changed`, even when all three lists are empty.

**A rejected candidate changes nothing.**
`config.reload` on a file that does not load answers `-32007` with `{path, line?, message}` and publishes a `config.invalid` event carrying the same three fields, so a caller and a watcher render one diagnostic.
The previous snapshot stays live, its runtimes keep serving and keep their engine locks, and the revision does not move.

**`config.init` and `config.add_account` carry no secret and validate before they write.**
The candidate document is built in memory, validated, and only then written, so a refusal leaves `config.toml` byte-identical.
`config.init` refuses an existing file with `-32602` naming it, because `mp config init` asks "Overwrite? [y/N]" and a daemon has nobody to ask; `config.add_account` refuses a missing file with `-32602` naming `config.init`, and a duplicate account name with `-32602` naming the name.
The daemon-era equivalent of the wizard's password prompt is a second call to `config.set_password`: one path into the secrets backend is one path to audit.

The two wizards are the exception, and the one place a client still writes a secret itself: `src/config_cmd/init.rs` calls `secrets::set_secret` directly (lines 227, 286, 633 and 673) for the passwords it prompts for, because the prompting loop has no wire shape and its intermediate answers steer the next prompt. It is one of the seventeen residue rows `tests/architecture_boundaries.rs` records, and it goes when the wizard protocol lands.

**`config.set_password` publishes no event.**
A stored password changes nothing a client can observe, because `config.get` said `<redacted>` before and says `<redacted>` after; its `affected` names `account:<name>` so a client holding a per-account view re-reads what depends on credentials.
The key is the one the existing backend already uses, `smtp-password-<account>` or `imap-password-<account>`, so the daemon and the pre-daemon binary agree about where a password is.
An unknown account is `-32005` with `{account}`, and neither its message nor its payload echoes the value: an error message is the single most likely place for a secret to escape.
The backend is opened on first use rather than at startup, because a first run has no configuration to select one from; a daemon that has already opened one keeps it, so changing `secrets_backend` takes a restart.

### The `draft.*` family

The daemon watches every account's drafts directory and the signatures directory, so a draft written by `$EDITOR`, by an agent or by the daemon itself reaches every client as an event without anybody asking.
The watcher is described in [daemon-operations.md](daemon-operations.md); what it produces on the wire is the three kinds below and the snapshot rows above.

The family is the ten methods below, all served from protocol 1 and all durable: a draft written half way because its caller hung up is what this family must never produce.

| method | kind | params | result |
|---|---|---|---|
| `draft.approve` | command | `{account, id}` | `{account, id, status: "approved", path}` |
| `draft.create` | command | `{account, name, no_signature?, signature?}` | `DraftCreated` |
| `draft.demote` | command | `{account, id}` | `{account, id, status: "draft", path}` |
| `draft.discard` | command | `{account, id\|selector, force?}` or `{account, sent: true}` | `{account, id, selector, status}` or `{account, cleared, kept}` |
| `draft.forward` | command | `{account, source, no_signature?, signature?}` | `DraftCreated` |
| `draft.list` | query | `{account, status?}` | `DraftListing` |
| `draft.path` | query | `{account, id\|selector}` | `DraftLocation` |
| `draft.preview` | query | `{account, id\|selector}` | `DraftPreview` |
| `draft.reply` | command | `{account, source, all?, no_signature?, signature?}` | `DraftCreated` |
| `draft.validate` | query | `{account, id?\|selector?}` | `DraftValidation` |

The result types are `mp_protocol::draft`, beside `mp_protocol::events`: they are wire shapes, so they live in the crate a client links rather than in the daemon crate a client must never link.
`DraftCreated` is `{account, id, selector, path, source}`, whose `source` is `{id, selector}` for a reply or a forward and absent for a draft made from nothing.
`DraftListing` is `{account, drafts, skipped, collisions}`, whose rows are `{id, selector, path, status, to, subject, valid, ready}` in the index's order (`mtime DESC, id ASC`); `to` and `subject` stay nullable, which is where the row differs from the snapshot's, and `skipped` names a file that will not parse by path because such a file has no id to be named by (#0080).
`DraftValidation` is `{account, reports}`, whose reports are `{id, selector, valid, error, warnings}`.
`DraftLocation` is `{account, id, selector, path, status}` and `DraftPreview` is the dry run's record, whose body is cut at 500 characters while `body_truncated` is decided on 500 bytes and whose `signature` is `null` for the CLI, because the body already carries it (#0099).

**`draft.path` is the family's resolver.**
It takes what the user typed (`<id>`, `drafts/<id>`, `mp://<account>/drafts/<id>`) and answers the canonical selector, the canonical path and the current status; which *account* a selector names stays a client-side decision, because `Selector::parse` needs no store.
The status is there because `mp mark-approved` needs the *previous* one to choose its line, and the approval's own four keys are frozen.

Every query answers from a fresh scan of the account's drafts directory rather than from the watcher's settled inventory, which is what makes a draft written a millisecond ago addressable: the pre-daemon binary rebuilt the index at the start of every command, and a resolution that waited for a poll plus a debounce would answer "no such draft" for up to a second.
The scan takes no engine lock and opens no store, exactly as the watcher's lookup did.
The two mutators fall back to that inventory when the scan finds nothing, because a file that will not parse has no `id:` and is announced under its stem, which is the id `draft.approve` refuses `draft_invalid` for.

**`draft.discard` removes one draft, or sweeps the sent ones.**
A draft is local, so there is no server op and no backend: just the file and the index row the next scan drops (#0073).
`force` is required for an `approved` draft and for nothing else, because an approved draft is a queued send and deleting it drops that send; a `sent` draft needs none, since retiring one is the whole point of the sweep.
The refusal is `delete_indexed_draft`'s own sentence, verbatim, and so is the refusal of a draft an outbox row still holds mid-send.
The answer's `status` is the status the draft was in, which is what was discarded.

The `--sent` sweep is a parameter of the same method rather than a method of its own, because one verb over a set is the same verb: `{account, sent: true}` clears every `sent` draft of one account and takes no selector, answering `{account, cleared, kept}` where `kept` is `[{id, selector, error}]`.
The sweep keeps going past a file it cannot remove, so a client prints one line per survivor before its own summary.
`sent: true` beside `id`, `selector` or `force` is `-32602`: a sweep names no draft, so a caller who named one disagrees with themselves.
A sweep invalidates `draft:<account>` rather than one row, which is the family's one command that names no draft.

`draft.approve` and `draft.demote` rewrite the `status:` line and nothing else, re-serialising no frontmatter, so a field the daemon does not model survives.
They publish no event of their own: the watcher notices the daemon's write like any other and publishes the `draft.changed` that carries the new state, so an approval over the socket and an approval in an editor look identical from the outside.

Every `path`, `kept` and `shadowed` field is absolute and under `<data_dir>/accounts/<account>/drafts/`, and nothing in the family names the store, the blobs or the runtime directory.

An unknown account is `-32005` with `{account}`; an account with no store is `-32006` for the two methods that read one (`draft.reply`, `draft.forward`) and never for the seven that read the directory, because a drafts directory is local truth and an account that has never synced still has one.
An id nothing resolves to is `-32602` with `{account, id}`; a name `draft.create` would overwrite is `-32602` with `{account, name, path}`; a draft already `sent` is `-32602` with `{account, id, status}` for both mutators; a draft that will not parse is `-32010` `draft_invalid` carrying the `draft.invalid` payload.
An invalid draft is not a refusal: `draft.validate` reports it and the exit code stays the client's.

### The `sync.*` family

Five methods carry the four commands that need a mail server: `sync.quick`, `sync.full` and `sync.watch` here, `mailbox.list_server` beside `mailbox.list`, and `message.list_server` beside the read slice's three.

```text
sync.quick          {account, limit?, mailbox?: [str], dry_run?}  -> {operation_id}
sync.full           {account, mailbox?: [str], dry_run?}          -> {operation_id}
sync.watch          {account, mailbox?}                           -> {operation_id}
mailbox.list_server {account}
        -> {account, source, mailboxes: [{name, delimiter, attributes, total, unread}]}
message.list_server {account, mailbox, limit, criteria?}
        -> {account, mailbox, messages: [{from, to, cc, subject, date, has_attachments, body}]}
```

`sync.quick` is the newest UIDs per mailbox and takes a `limit`; `sync.full` is everything the mailbox lists and takes none, because a bounded full pass is a quick pass under another name and a caller who sent one is told rather than quietly given a different pass.
Both are operations rather than commands, and both are `durable`: a sync a GUI started must keep running, and stay watchable, from the CLI window beside it.
`sync.watch` is the exception and is `client_scoped`, because a watch exists to answer one client's question and an interrupted `mp watch` must not leave the daemon holding an IDLE on its behalf.

`account` is a required string on all three and there is no `all_accounts`: a client that syncs several accounts issues one operation per account, in configuration order, because the per-account header, the failure denominator and the exit code are all rendering of a per-account result.
An account that configures neither IMAP nor SMTP is `-32006` with `{account, state: "local_only"}` rather than an operation that does nothing, and a client renders that as its skip line without changing the run's exit code.
An unknown account is `-32005` with `{account}`.
Neither pass requires a local store: a sync is what gives an account one.

A `mailbox` a pass cannot resolve fails the **operation** with `-32602`, naming the account and listing the mailboxes it does know, so a client renders it beside every other per-account failure rather than through a second path.
The refusal arrives before anything opens a socket, and before either drain runs.

A pass whose account is already another engine's *succeeds* and answers `{blocked: true, outcome: null}`: the holder is doing the work, so nothing ran, no session was opened and that is not this operation's failure (#0122).
A pass that ran answers `{blocked: false, outcome: <sync.completed payload>}` and publishes the same payload as a `sync.completed` event, unless it was a dry run, whose counts describe mail that was not ingested and would read to every other client as mail that arrived.

Each of the five slots of a tick is one `operation.progress` report whose `phase` is one of `head_outbox`, `head_mutations`, `body`, `tail_outbox`, `tail_mutations`, in that order (#0114).
For the four drain phases, `done` is what the drain completed and `total` what it left behind - still-pending sends, or rolled-back mutations - and both are `null` when the drain had nothing a user would want told.
`message` carries the reason a drain could not run at all, which is a warning and not a failure: the queue is retried on the next tick, so a drain that could not run may not turn a sync that worked into a failed command.
A client that labels a tail report derives the label from the phase name and from nothing else.

`mailbox.list_server` and `message.list_server` are queries against the server, so they take no engine lock and read no store.
`source` is `imap` or `graph`, because the two transports report different things about a mailbox: Graph's folder list carries `total` and `unread`, and IMAP's `LIST` carries `delimiter` and `attributes` and no counts, so the members the server said nothing about are `null` rather than a number nobody measured.
`message.list_server` requires `mailbox`: a server query with no mailbox names none, and a default is the client's business.
Its `criteria` object takes `from`, `to`, `cc`, `subject`, `body`, `since`, `before` and `message_id`, all optional.
Neither answer names a file, and `message.list_server` writes nothing: mail enters the store through a sync, which is the only path that fetches by UID and can key a row (#0037).

This build's watcher is INBOX-only.
`sync.watch` accepts `mailbox` absent or `INBOX` and refuses anything else with `-32602` and `{account, mailbox}`; a client narrows before it calls and says so.
The watch is validated before an id is issued - the account, the mailbox, the transport, the credentials - so a refusal is the call's error rather than an operation that fails immediately, and it succeeds with `{mailbox, changed: true}` the first time the mailbox changes.
A Graph account has no IDLE to offer and is `-32603`.
No timeout crosses the socket: a client that wants to stop waiting calls `operation.cancel`.

### Long-running operations

A method whose kind is *operation* answers at once with `{"operation_id": str}` and does the work in the background.
The id is an opaque non-empty string, unique for the life of the daemon process: a client echoes it and never parses it.

An operation is in one of five states, `queued`, `running`, `succeeded`, `failed` or `cancelled`, and it moves forward only: `queued -> running -> {succeeded, failed, cancelled}`, plus `queued -> {succeeded, failed, cancelled}` for work that finishes before it reports anything.
The last three are terminal, and a transition out of a terminal state is a silent no-op: the loser of the race is a worker that was already told to stop, and neither a crash nor a second finished event is an answer to that.
A progress report on a queued operation moves it to `running`, because a report is evidence of running.

An operation is daemon-wide rather than connection-private: any initialized connection may read any live id, and its events reach every bootstrapped connection.
The daemon serves one user's data directory, and a GUI that started a sync must be watchable from the CLI window beside it.

`operation.status` takes `{"operation_id": str}` and returns:

```json
{
  "operation_id": "8f2c…",
  "method": "sync.quick",
  "state": "running",
  "scope": "durable",
  "progress": {"phase": "body", "done": 42, "total": 214, "message": null},
  "result": null,
  "error": null
}
```

Every member is present, and the three that have nothing to say are `null`.
`method` is the method that started the operation and `scope` its declared `cancel_scope`.
The newest report survives the finish, so a client that missed the last progress event can still read it.
`result` is what a succeeded operation produced, `error` the `{code, message, data}` object a failed or cancelled one carries, and never both.
The same object is one entry of the bootstrap snapshot's `operations` array, which lists every operation this daemon has not settled, in start order.

`operation.cancel` takes `{"operation_id": str}` and returns `{"operation_id": str, "state": "cancelled"}`.
It is synchronous and terminal at once: it shuts the operation's token, settles the state and publishes the finished event before it answers, so the `operation.status` a client calls immediately afterwards already says `cancelled`.
The worker observes its token whenever it next looks, and its later result is the ignored invalid transition above.
The answer is therefore a fact and not a promise.

Cancelling an operation that has already finished is `-32602` with `data` of `{operation_id, state}`, naming the state that made the cancel impossible, and both `operation.cancel` and `operation.status` refuse an id the daemon never issued with `-32602` and `data` of `{operation_id}`.
`-32008` `operation_cancelled` is not used for either: it is the operation's own answer to its caller, and one code may not mean two things on one connection.

Progress travels as the lifecycle event kind `operation.progress`, with a payload of `{operation_id, phase, done, total, message}`, where `total` and `message` are `null` rather than absent when there is nothing to say: a client reads `total` to draw a bar and has to tell "unknown" from "missing field".
Every report is its own event and none coalesce, so `done` never skips a step.

The terminal transition travels as `operation.finished`, with a payload of `{operation_id, state, result}` for a success and `{operation_id, state, error}` for a failure or a cancellation.
The absent member is absent rather than null.
A cancelled operation's error is the table's own `-32008` with `{operation_id}`, so a client that missed the `operation.cancel` response learns the same fact from the event.
Starting an operation and moving it to `running` publish nothing: a client learns that an operation exists from the answer that carried its id.

Both kinds are lifecycle events, so they survive a queue overflow and a poisoned queue: no snapshot brings a progress report back.
They take an ordinary state revision, from the same counter every committed change takes one from, so a client compares them against its watermark like any other event.

When a connection closes, the daemon cancels the non-terminal operations that connection started **and** declared `client_scoped`, in start order.
Its durable operations keep running, another connection's work of either scope is untouched, and its own finished operations are left alone.
A cancellation from a disconnect is indistinguishable from an explicit `operation.cancel`: same state, same finished event, same `-32008` inside it.
A second code for "your socket went away" would be a distinction only the daemon can see.

## Error codes

JSON-RPC's own codes keep their meanings: `-32700` parse error, `-32600` invalid request, `-32601` method not found, `-32602` invalid params, and `-32603` internal error.
The daemon's conditions occupy `-32010` to `-32000`.

| code | name | `data` |
|---:|---|---|
| -32000 | `not_initialized` | `{}` |
| -32001 | `identity_mismatch` | `{daemon: {data_dir, config_dir}, client: {data_dir, config_dir}}` |
| -32002 | `protocol_incompatible` | `{daemon: {min, max}, client: {min, max}}` |
| -32003 | `capability_missing` | `{missing: [..]}` |
| -32004 | `frame_too_large` | `{limit, seen}` |
| -32005 | `account_unknown` | `{account}` |
| -32006 | `account_not_ready` | `{account, state}` |
| -32007 | `config_invalid` | `{path, line?, message}` |
| -32008 | `operation_cancelled` | `{operation_id}` |
| -32009 | `shutting_down` | `{}` |
| -32010 | `draft_invalid` | `{account, id, path, diagnostics: [{line, message}]}` |

`identity_mismatch` names both directories on both sides so the client can print all four and tell the user which override to drop.
`frame_too_large` reports the cap that was breached and the byte count that breached it, the same pair the decoder produces.
`message` is a human-readable line for the log and the CLI, and clients match on the code, never on the message text.

`draft_invalid` is its own code rather than an overloaded `-32602`, because "you asked for a draft that does not exist" and "the draft you asked for will not parse" may not be the same answer on one connection, and its `data` is the `draft.invalid` payload, so a client renders the caller's refusal and the watcher's event with one piece of code.

The read-only methods use four of these, and `mailbox.list` uses the same two account refusals `message.list` does.
An account no configuration names is `account_unknown`, carrying the name that was asked for; a configured account with no readable store is `account_not_ready`, carrying the same `state` `account.list` reports for it, so two answers about one account cannot contradict each other.
A mailbox the account does not have is `-32602` naming the ones it does, because the caller asked for something that does not exist rather than for something the daemon refuses.
A store that exists and then fails to open or to read is `-32603`.

The CLI maps two of these onto stable exit codes: `3` for an incompatible daemon, which prints the `mp daemon restart` command, and `4` for a daemon that is unavailable or failed to start, which prints the socket path, the daemon log path and the `mp daemon run` command.
Exit 4 is also what a client reaches after an on-demand start it asked for did not produce a daemon within its bound; the start policy and its bound are in [daemon-operations.md](daemon-operations.md#on-demand-start-and-the-no-daemon-list).

## Event semantics

State changes reach clients as the `state.event` notification, whose `params` is an event envelope of `{instance_id, revision, kind, payload}`.

`instance_id` is the daemon instance that produced the event, and a client that sees an unfamiliar instance has been reconnected to a new daemon and must bootstrap again.
`revision` is the monotonic state revision the event moves the client to, and `0` is the pre-bootstrap sentinel that never appears on the wire.
`kind` selects the client's handler, for example `account.state_changed`.
`payload` is an object so a kind can gain fields without a version bump.

The revision counter is dense: it is strictly increasing within an instance and every committed change takes the next number, so no number is ever skipped on the daemon's side.
What one connection receives is strictly increasing but not necessarily dense, because coalescing merges two queued events into one that carries the newer revision and the older number never travels.
Delivery order is therefore the contract, and the daemon says so itself when something was lost: a `state.resync_required` notification, never a hole a client has to infer.

A client compares each event against its watermark and does one of four things: an event above it is applied and moves it; one at or below it is a duplicate the snapshot already carries and is dropped without a word; one from an unfamiliar instance is refused whatever its number, because revisions are only comparable within the daemon process that issued them; and a `state.resync_required` poisons the stream until a fresh `state.bootstrap` clears it.
An instance change poisons it the same way: a client that kept applying past either would build a state nothing on the daemon's side corresponds to.

`mp-client`'s `StateTracker` is stricter than that today and treats any jump above `watermark + 1` as a gap, which is exact only on a stream that coalesced nothing.
The two agree in this build, because Phase 3a registers no method that commits a change and the only producer is the burst hook, whose events name distinct resources on purpose.
Phase 3b's `sync.completed` does not change that: it is a lifecycle event, it takes its revision from the same counter under the same gate as every committed change, and it merges with nothing, so a run of outcomes reaches a client dense and `StateTracker` applies every one of them.
Reconciling them, by having the daemon carry the highest revision a merged entry superseded or by dropping the arithmetic in favour of the daemon's own control message, is a Phase 3b item and is in `BACKLOG.md`.

### Event kinds

An event either replaces a small resource whole or names one whose cached answer went stale, and the kind says which.

`account.state_changed` is the kind an account's readiness travels as, with a payload of `{account, state}` where `state` is the `opening`/`ready`/`blocked` the snapshot uses, plus a `reason` when it is `blocked`.
It is what converges a snapshot taken while an account was still `opening`, and it needs no second bootstrap.
`draft.changed` replaces one draft whole, with a payload of `{account, id, path, to, subject, status, valid, ready}`, the snapshot row plus its account.
`draft.invalid` replaces the *same* resource, `draft:<account>/<id>`, with a payload of `{account, id, path, diagnostics}`, where a diagnostic is `{line, message}` and `line` is 1-based, file-relative and `null` for a refusal by value rather than by syntax.
It is a replacement rather than a lifecycle event because a broken draft is a fact about a resource that stays true until somebody fixes the file: it reduces into the snapshot, so a client that connects after the breakage still learns about it, and fixing the file replaces the row rather than adding a second one.
All three are replacements because they are small and a client that re-queried them would learn nothing the payload did not already carry.

`signature.changed` says that a signature file was written, with a payload of `{name, path}` where `name` is the file stem.
It is a lifecycle event and reduces into no snapshot: signatures are global rather than per-account, so no per-account change can carry one, and the event is a "re-read the list" for a client that caches signature bodies.
A deleted signature publishes nothing in this build.

`state.invalidate` says that a cached query went stale, with a payload of `{resource, scope}`: `resource` is what to re-read, as the `family:path` a method's `affected` list uses, and `scope` is the identity of the query whose answer is now wrong.
Mailbox and outbox counts travel this way, as `{"query": "counts"}` over `mailbox:<account>/<slug>` and `outbox:<account>`, because that is what makes them coalescible: a hundred count changes for one mailbox are one thing to re-read.

`state.remove` says that a resource is gone, with a payload of `{resource}`.
A removal is a fact about a moment and is never merged with anything, in either direction, including another removal of the same resource.

`operation.progress` and `operation.finished` are the two lifecycle kinds of the operation family, described with the long-running operations above.
`sync.completed` is the third lifecycle kind, described below.
`config.changed` and `config.invalid` are the fourth and fifth: `config.changed` carries `{added, updated, removed, config_revision}` and closes every successful swap, `config.invalid` carries `{path, line, message}` and is the diagnostic a rejected candidate publishes.
Both are lifecycle events, so two swaps never coalesce into one: the lists are the whole payload, and merging them would hide the first swap's from a client that was slow to read.
An account removed by a swap travels as `state.remove` of `account:<name>` and needs no kind of its own.
The daemon's own `Change` type also names `mailbox.counts_changed`, `draft.removed` and `outbox.counts_changed`, which are not wire kinds: those changes travel as `state.invalidate` and `state.remove`, and the kinds listed here are the whole of what a client sees.

### `sync.completed`

One sync tick finished, and the payload is everything it did.
A tick is a command outcome rather than a resource's state, so the event names no resource, reduces into no snapshot, and a client that bootstraps between two ticks learns about neither.

| field | type | meaning |
|---|---|---|
| `account` | string | the account the tick belongs to |
| `severity` | `"ok"` \| `"warning"` \| `"error"` | how a client presents it |
| `saved` | u64 | messages ingested as new rows |
| `skipped` | u64 | messages the store already held |
| `flags_updated` | u64 | rows whose flags were updated from the server |
| `pruned` | u64 | rows deleted because the server no longer lists their UID |
| `prunes_deferred` | u64 | rows found vanished and not deleted, because the pass came back short |
| `uid_rebound` | u64 | rows rebound to a new UID after a UIDVALIDITY reset |
| `uidvalidity_resets` | u64 | mailboxes refetched in full after a UIDVALIDITY mismatch |
| `bodies_truncated` | u64 | mailboxes stopped at the body-fetch deadline with mail still to download |
| `non_converging` | array of string | mailboxes that downloaded the same UIDs again, sorted and deduplicated |
| `failed_mutations` | u64 | queued mutations that failed and were rolled back |
| `error` | string or null | the engine's error rendered with `{:#}`, `null` on a tick that ran |

Every key is present on every payload, so a client never branches on an absent one, and a tick that did not fail carries `"error": null` rather than an empty string.

The severity is decided in this order: an `error` that is not null is `error` whatever else the tick did; otherwise a non-empty `non_converging` **or** a `failed_mutations` above zero is `warning`; otherwise the tick is `ok`.
`bodies_truncated` and `prunes_deferred` never raise it: a deadline stop is progress, because the mailbox has more mail on the server and the next tick resumes from the same cursor, and a deferred prune is a suspended deletion rather than a failure.
The daemon decides the severity and the wire carries it; a client applies what it was sent rather than deriving its own, because a client that recomputed could disagree with the daemon after a rule change and present a warning tick as clean.

The payload carries no formatted string.
The TUI's status line and `mp sync`'s lines are derived from it by `mp_client::format::sync_status_line` and `mp_client::format::sync_cli_lines`, which are pure functions over the payload and hold every wording in one place.
The same module holds the drain report lines a pass's `operation.progress` reports render to, `outbox_drain_line`, `mutations_drain_line` and `drain_failed_line`, and the `TAIL_LABEL` a tail phase earns.

`sync.completed` is a lifecycle event and coalesces with nothing.
Two ticks are two facts about two moments: merging an earlier warning into a later clean tick would present that tick as clean, which is exactly what the severity exists to prevent.
A queued outcome therefore also survives a queue overflow's discard, because no re-bootstrap would bring it back.

### Delivery, coalescing and caps

`state.bootstrap` returns a snapshot and the revision it was taken at, as one serialised operation.
Events with a higher revision are queued during that operation and released in revision order after the response frame, so a client applies every change exactly once.
An event frame and a response frame never interleave: a connection writes one whole frame at a time and answers a request before it sends an event.
A second `state.bootstrap` on one connection empties that connection's queue: everything in it predates the snapshot the client is about to apply.

Each connection has its own outbound queue, and equivalent events coalesce in it while they wait.
A `state.invalidate` merges with a queued one for the same resource and the same scope; a replacement merges with a queued one of the same kind for the same resource, or, for a kind that names no resource, with any other of that kind.
The merged entry takes the newer payload and the newer revision and moves to the tail, so what a client receives is still in revision order.
A `state.remove` and a lifecycle event merge with nothing.

The queue holds at most **512 events** or **4 MiB** of payload, whichever binds first.
A client that stops reading therefore costs the daemon a bounded amount of memory and delays nobody: connections are served independently, and a blocked write never holds up the state or another client.

When a push would exceed either cap the queue overflows: every queued domain event is discarded, the queue is poisoned, and one `state.resync_required` notification is sent with `params` of `{instance_id, reason}`, where the reason is `event_queue_overflow`.
A poisoned queue accepts no further domain event until the client calls `state.bootstrap` again, which clears the poison: sending more would build a state nothing on the daemon's side corresponds to.
Lifecycle events are the exception and survive both the discard and the poison, because no snapshot carries them and a re-bootstrap would not bring them back.
They do not survive the re-bootstrap itself: a second `state.bootstrap` empties the whole queue, lifecycle events included, so a progress report queued behind a resync is lost where the same report queued behind an overflow is kept. That is an inconsistency of this build rather than a rule, recorded in ticket #0121 and in `BACKLOG.md`.
If the control notification itself cannot be written, the daemon closes that connection and keeps serving every other one.

A client that receives `state.resync_required` discards its state and calls `state.bootstrap`.

## Fixtures

`crates/mp-protocol/fixtures/` holds one committed example per public shape, and `tests/daemon_protocol_fixtures.rs` checks them on every run.

A fixture is stored canonically, which means `serde_json::to_string_pretty` of its own content plus a trailing newline: two-space indentation and alphabetically sorted keys.
A diff on a fixture is therefore a protocol change and never a reformat.

The file name selects the type a fixture must parse into: `error.*` is an error response, `event.*` a bare event envelope, `notification.*` a notification, `*.request.json` a request, and `*.response.json` a response.
Parsing a fixture and serialising it back must reproduce the file's JSON exactly, which is what catches a field the type forgot or invented.
A new fixture whose name matches no rule fails the suite rather than being skipped.

## Protocol changelog

### Version 1

Initial version, introduced with the daemon in #0120 and completed in #0121.
One entry, because nothing shipped against version 1 before it was whole: the two tickets are phases of one migration and no client outside this repository has spoken it.

**Transport.**
Newline-framed JSON-RPC 2.0 over a Unix-domain socket, with a 1 MiB request frame cap inclusive of the terminator.
A response above the 16 MiB response cap is a `frame_too_large` error carrying `{limit, seen}`, the same pair an oversized request earns.

**Handshake and lifecycle.**
The `initialize` handshake with the client, protocol, capabilities, and identity params, and the daemon, protocol, instance, capabilities, platform, lifecycle, and config-status result.
A `client.type` outside `cli`, `tui` and `gui` is `-32602`, because a method that branches on the caller may not be handed a fourth kind.
The `daemon.status` and `daemon.stop` methods, reachable before the handshake, and the `not_initialized` gate on everything else.
The advertised capability list is the two lifecycle methods followed by the dispatcher's table in method-name order, derived rather than written out.

**Errors.**
The error table above, codes `-32000` to `-32010`.

**Methods.**
The read-only `account.list`, `mailbox.list`, `message.get`, `message.list` and `message.search`, all behind the handshake and none taking an engine lock.
`message.get` addresses one message by `"<mailbox>/<uid>"` or by the `mp show` selector grammar, exactly one of the two, and returns the `mp show --json` record with `body` omitted when it was not asked for and `null` when the store cannot produce one; `message.search` mirrors the `mp search --local` flags, names `--body` `body_query` on the wire, and answers in the store's ranking order; `message.list` gained `projection`, whose `envelope` value returns the `mp dump-mailbox --json` records of one account across every selected mailbox, `mailbox` accepting a name, an array or `null`.
`account.list` reports the account states `ready` and `blocked` from a read-only store probe; `mailbox.list` returns the sidebar hierarchy of one account with the three counts `total`, `unread` and `badge` per mailbox; `message.list` spells an unlimited listing as `null`, refuses a mailbox the account does not have with `-32602`, and carries both dates per row, the derived `date_sort` and the stored `date_display`, so a listing renders from the wire alone.
The `state.bootstrap` method, whose result carries the negotiated `capabilities` of the calling connection, a `revision` that is never `0`, and a snapshot of `accounts`, `mailboxes`, `drafts`, `outbox`, `holds`, `operations` and `diagnostics`, with one key per account in the three maps.
Its `operations` array lists the daemon's unsettled operations in start order, each rendered as an `operation.status` result.
The `operation.status` and `operation.cancel` methods, the five operation states, `-32602` for a cancel or a status on a terminal or unknown operation, the `cancel_scope` declaration of `durable` or `client_scoped`, and the rule that a disconnect cancels only that connection's non-terminal client-scoped work.
Every method declares a kind (`Query`, `Command`, `Operation`, `ClientIntegration`), a `since` of at least `1`, and a `cancel_scope`.

**State and events.**
The `state.event` and `state.resync_required` notifications, and the `{instance_id, revision, kind, payload}` event envelope.
The register-before-capture ordering and the watermark that drops every revision at or below the captured one, which together make a change around a bootstrap arrive exactly once.
The event kinds: `state.invalidate` `{resource, scope}` and `state.remove` `{resource}`, the replacements `account.state_changed` `{account, state}`, `draft.changed` `{account, id, path, to, subject, status, valid, ready}` and `draft.invalid` `{account, id, path, diagnostics}`, and the lifecycle kinds `operation.progress`, `operation.finished`, `sync.completed`, `config.changed`, `config.invalid` and `signature.changed` `{name, path}` with the payloads above.
The `draft.*` family, which is the ten methods above with the `mp_protocol::draft` result types, the fresh-scan resolver behind `draft.path`, and the `draft_invalid` refusal `draft.approve` introduced; `draft.discard` `{account, id|selector, force?}` -> `{account, id, selector, status}` and its sweep `{account, sent: true}` -> `{account, cleared, kept}` joined it in P4-U8, with `force` required for an `approved` draft alone and `sent` beside any address `-32602`.
The two message mutations, `message.archive` and `message.delete` `{account, id|selector, mailbox?}`, both durable commands that resolve the backend before they touch the store, drain the op they owe the server before answering, and report a missing credential as `-32603` with `{account}`.
The three handle methods, `message.materialise_attachment` `{account, id|selector, mailbox?, part}` and `message.materialise_html` `{account, id|selector, mailbox?}` -> `{handle, path, name, bytes, expires_at}`, and `message.release_handle` `{handle}` -> `{}`, with the `"<mailbox>/<uid>"` message id, the ten-minute default lifetime, the `<data_dir>/runtime/handles/<handle>/<name>` layout, and the pin that keeps the retention sweep off a blob a client has open; the `{selector, mailbox?}` addressing and the `name` field were added in P4-U8, both additive.
The snapshot's draft row, `{id, path, to, subject, status, valid, ready}`, which a `draft.invalid` reduces into with `status: "invalid"` and `valid: false`.
The coalescing rules above, the 512-event and 4 MiB per-connection caps, `event_queue_overflow` as the reason an exceeded cap resyncs a client, and the survival of lifecycle events across a discard and a poison.
The `sync.*` family, which is `sync.quick` and `sync.full` as durable operations over `{account, limit?, mailbox?, dry_run?}` (`limit` on the quick pass alone) and `sync.watch` as a client-scoped one over `{account, mailbox?}`, joined in P4-U10 by `mailbox.list_server` `{account}` and `message.list_server` `{account, mailbox, limit, criteria?}`; with them the five phase names `head_outbox`, `head_mutations`, `body`, `tail_outbox` and `tail_mutations` that an `operation.progress` of a pass carries, the `{blocked, outcome}` result, the `local_only` account state on `-32006`, and the INBOX-only refusal of `sync.watch` with `{account, mailbox}`.
The `send.*` family, which is `send.approved` `{account}`, `send.draft` `{account, selector|id}` and `send.invite` `{account, subject, start, to?, cc?, end?, duration?, location?, description?, uid?, signature?, no_signature?}` as durable operations answering with `{operation_id}`, `send.outbox_retry` `{account, row_id}` as a fourth, `send.outbox_list` `{account}` as a query and `send.outbox_discard` `{account, row_id}` as a command; their result types are `mp_protocol::send` (`SendOutcome`, `ApprovedOutcome`, `OutboxListing`, `OutboxRow`, `OutboxCounts`, `OutboxRetryOutcome`, `RecipientOutcome`, `SentCopy`) and every one of them is path-free.
None of the three sends takes a `hold`, a `hold_secs` or a `countdown`, and a caller that sends one is `-32602`: the CLI send paths bypass the undo-send hold (`ANO-7`) and Phase 6 must add a method of its own rather than a parameter here.
`send.invite` refuses a Graph account with the `ANO-4` sentence before it looks at anything else about the invitation, carrying `{account}`; `uid` is the client's, so the UID a user read in the preview is the UID that goes out, and `signature`/`no_signature` are the two global CLI flags, since an invitation has no draft body to carry a signature in.
`send.outbox_list` answers `ever_used: false` for an account with no store rather than refusing, because "nothing has been queued yet" is a fact a client renders and not an error.
`send.outbox_retry` accepts a `failed` row, which it re-arms, and a `sent_pending_append` row whose APPEND has already been attempted, which it re-drives behind the Message-ID dedup search; anything else is `-32602`, except while another retry for the same account is running, when the row's state is that retry's to decide and the second call is admitted and settles on what it finds.
The admin slice (P4-U14) added three families' worth of methods and three to `config.*`, every one of them since 1 and `durable`, and every one taking a required `account` with no `all_accounts`: the loops and the "default account" are the client's, over `config.get`'s list in configuration order.
`contact.search` `{account, query, limit}` -> `{account, query, contacts}` and `contact.stats` `{account}` -> `{account, total, sent_to, sent_cc, received, built_at, cache_path, top}` are queries over the frecency index, `top` holding at most ten rows; `contact.rebuild` `{account}` is an operation reporting `{phase: "contacts", message: "<account>"}` and settling `{account, contacts, kept, saved, cache_path}`, where `saved` is `written`, `refused_empty` or `refused_shrunk` (#0067).
`calendar.rebuild` `{account}` settles `{account, resolved, invites_seen, replies_seen, cancelled}` and writes nothing; `calendar.rsvp` `{account, selector, mailbox?, response}` settles `{account, selector, response, subject, organizer, message_id, delivered}`, `response` being exactly `accept`, `tentative` or `decline`, and refuses a Graph account with the `ANO-4` sentence and `{account}` before it examines the selector.
`diagnostic.store_gc` `{account, dry_run, force}` settles `{account, dry_run, cap_bytes, before_bytes, after_bytes, evicted_bytes, evicted, decision}`, the decision being `{kind: "under_cap", cleared_marker}`, `{kind: "warned_first_breach"}`, `{kind: "refused_too_much", would_evict_bytes}` or `{kind: "evicted"}`.
`config.cutover` `{account, dry_run}` settles `{account, dry_run, drafts: {imported, already_indexed, skipped, collisions}, remnants: [{path, md_files, bytes}]}`, with `skipped` and `collisions` already rendered as the sentences the report prints; `config.oauth2_login` `{account}` reports `{phase: "device_code", done: 0, total: null, message: "<verification_uri> <user_code>"}` and settles `{stored, account, kind, key}`, the browser launch staying client-side (`INT-04`); `config.reset_secrets` `{}` -> `{removed: [path]}` is a command naming the secrets file first and then the token caches, in path order.
Four of these results carry a path deliberately, because the command prints one and a client cannot compute it: `contact.stats.cache_path` and `contact.rebuild`'s settled `cache_path`, `config.cutover`'s `remnants[].path` and `drafts.imported[]`, and `config.reset_secrets`'s `removed[]`, beside `config.get`'s `path`.
`config.get`'s `config` is the *effective* configuration, which means after serde defaults and not after the engine's clamps: a configuration that omits `smtp.port` reports `465` and one that omits `imap.port` reports `993`, where the pre-daemon binary printed `0` for both.
