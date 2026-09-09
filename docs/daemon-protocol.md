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

- `client.type`, one of `cli`, `tui`, or `gui`, and `client.version`, the client's application version.
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
This build advertises `daemon.status`, `daemon.stop`, `account.list` and `message.list`, which are exactly the methods it serves: the list is the two lifecycle methods followed by every method registered on the dispatcher, derived at handshake time rather than written out, so a method cannot be served without being advertised or advertised without being served.
Requiring one this build does not have is a `capability_missing` at the handshake rather than a `-32601` at the first call, and an *optional* capability the daemon lacks is dropped from the connection's agreed set instead of refusing it.

The handshake happens once per connection, and a second `initialize` on the same connection is `-32600`.
The session state dies with the connection: it is never expired, reused, or transferred.

Any method other than `initialize` and the two lifecycle methods below issued before a successful `initialize` gives `not_initialized`, ahead of the method lookup, so an uninitialized client cannot probe which methods a daemon serves.
The refusal is per request rather than per connection: the connection stays usable and the `initialize` that should have come first still works on it.

## Method families

Methods are domain operations, and each one declares whether it is a query, a command, a long-running operation, or a client-side integration request.

### Method kinds

Every method registered on the dispatcher declares a kind, and the kind fixes what its answer carries beyond `result`: a `revision`, which is the daemon state revision the call moved to, and `affected`, the resources whose cached copies the call invalidated (`account:work`, `mailbox:work/inbox`, `message:work/inbox/41`).
Both are daemon-side facts and do not appear in the JSON-RPC `result`; they are what the daemon fans out as `state.event` notifications, so a client that applied an event never has to guess which of its caches went stale.

- **Query** reads and changes nothing, so its answer carries no revision and no affected resource. `account.list` and `message.list` are queries.
- **Command** changes state at once, so its answer carries the revision the change moved the daemon to and at least one affected resource. A command that changed nothing observable is a query, and a command with an empty `affected` would leave every client stale with no event to fix it.
- **Operation** runs long enough to be worth cancelling and observes a cancellation token. Cancelling is the method's own answer, `operation_cancelled` (`-32008`) with `{operation_id}`, never a cancellation imposed on it from outside: a method that has already committed a write reports the write rather than being reported as cancelled behind its own back.
- **ClientIntegration** is work only the client's process can do, such as opening a browser or revealing a file. The daemon answers with the instruction and the client carries it out.

A method also declares `since`, the first protocol version that served it, which is never below `1`.

`initialize`, `daemon.status` and `daemon.stop` declare no kind, because they are not dispatcher methods: they are lifecycle surface answered by the connection itself, ahead of the handshake gate and outside the domain.

- `state.*` for bootstrap and state diagnostics.
- `account.*` for listing, selection metadata, sync health, and account operations.
- `mailbox.*` for listings, counts, and mailbox metadata.
- `message.*` for listing, retrieval, search, selection, mutation, attachments, and browser materialisation.
- `draft.*` for creation, parsing status, validation, recipient editing, reply, reply-all, forward, approval, discard, and attachment changes.
- `send.*` for immediate send, approved batches, hold countdowns, cancellation, and outbox recovery.
- `sync.*` for quick sync, full sync, progress, and errors.
- `contact.*` for listing, ranking, rebuilding, and statistics.
- `calendar.*` for agenda queries, invitations, RSVP, updates, and cancellations.
- `signature.*` for list, read, create, update, rename, delete, and per-account default selection.
- `config.*` for safe reads, validation, updates, reload, account setup, authentication, and secret writes.
- `operation.*` for long-running operation status and cancellation.
- `diagnostic.*` for logs, health, and support information.
- `daemon.*` for status and graceful lifecycle control.

`initialize` is the one method outside a family, because it runs before any family gate exists.

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
`accounts` is empty until account runtimes exist, and an account state is one of `opening`, `ready`, `blocked`.
`mp daemon status --json` prints this object with a leading `"running": true`, or the same keys with null values and `"running": false` when nothing answers.

`daemon.stop` takes `{}` and returns `{"stopping": true}`.
The response is written and flushed before the shutdown starts, so the caller always learns the daemon accepted the request.
Open connections are not drained: the daemon unlinks its runtime files and exits, and a client that loses the socket mid-call reconnects.

### Read-only methods

`account.list` and `message.list` are the first domain methods, and they only read.
Both take the store path the CLI takes (`store::read`) and neither acquires the account's `EngineLock`: the daemon does not become an account's engine before Phase 5, so a running TUI or `mp sync` keeps the lock while the daemon answers listings beside it.

`account.list` takes `{}` and returns:

```json
{"accounts": [{"name": "work", "default": true, "backend": "imap", "state": "ready"}]}
```

It reports the configuration, not the runtimes: every `[[accounts]]` entry of `config.toml`, in the file's order, which is the order an `-A`-less command already treats as authoritative.
`default` is true for the first configured account and no other, because the CLI has no other notion of a default.
`backend` follows `auth_method` alone, `graph` for Graph and `imap` for everything else, and is independent of `state`.
`state` is decided on disk: an account whose store exists and opens is `ready`, and one with no store yet is `blocked`, since it cannot serve a read until `mp sync` writes one.
This build never reports `opening`: nothing here is asynchronous, so no account is ever between states.

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

`mp --daemon list-messages` renders from the wire alone: it opens no store of its own, and a client that never had one prints the same listing.

The `state` an account reports is probed read-only: the daemon opens the store file with `SQLITE_OPEN_READ_ONLY` and checks the schema stamp and the required tables, rather than going through `Store::open`, which creates a missing store and rebuilds a corrupt one.
Asking which accounts exist may not create or destroy a cache, so a file that fails the probe is `blocked` and is left exactly as it was found.


## Error codes

JSON-RPC's own codes keep their meanings: `-32700` parse error, `-32600` invalid request, `-32601` method not found, `-32602` invalid params, and `-32603` internal error.
The daemon's conditions occupy `-32009` to `-32000`.

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

`identity_mismatch` names both directories on both sides so the client can print all four and tell the user which override to drop.
`frame_too_large` reports the cap that was breached and the byte count that breached it, the same pair the decoder produces.
`message` is a human-readable line for the log and the CLI, and clients match on the code, never on the message text.

The read-only methods use four of these.
An account no configuration names is `account_unknown`, carrying the name that was asked for; a configured account with no readable store is `account_not_ready`, carrying the same `state` `account.list` reports for it, so two answers about one account cannot contradict each other.
A mailbox the account does not have is `-32602` naming the ones it does, because the caller asked for something that does not exist rather than for something the daemon refuses.
A store that exists and then fails to open or to read is `-32603`.

The CLI maps two of these onto stable exit codes: `3` for an incompatible daemon, which prints the `mp daemon restart` command, and `4` for a daemon that is unavailable or failed to start, which prints the daemon log path and the `mp daemon run` command.

## Event semantics

State changes reach clients as the `state.event` notification, whose `params` is an event envelope of `{instance_id, revision, kind, payload}`.

`instance_id` is the daemon instance that produced the event, and a client that sees an unfamiliar instance has been reconnected to a new daemon and must bootstrap again.
`revision` is the monotonic state revision the event moves the client to, and `0` is the pre-bootstrap sentinel that never appears on the wire.
`kind` selects the client's handler, for example `message.flags_changed`.
`payload` is an object so a kind can gain fields without a version bump.

`state.bootstrap` returns a snapshot and the revision it was taken at, as one serialised operation.
Events with a higher revision are queued during that operation and released in revision order after the response frame, so a client applies every change exactly once.
A gap in the revision sequence means the client missed an event and must bootstrap again.

When the daemon cannot preserve that guarantee, for example after an event queue overflow, it sends the `state.resync_required` notification with `params` of `{instance_id, reason}`.
A client that receives it discards its state and calls `state.bootstrap`.

## Fixtures

`crates/mp-protocol/fixtures/` holds one committed example per public shape, and `tests/daemon_protocol_fixtures.rs` checks them on every run.

A fixture is stored canonically, which means `serde_json::to_string_pretty` of its own content plus a trailing newline: two-space indentation and alphabetically sorted keys.
A diff on a fixture is therefore a protocol change and never a reformat.

The file name selects the type a fixture must parse into: `error.*` is an error response, `event.*` a bare event envelope, `notification.*` a notification, `*.request.json` a request, and `*.response.json` a response.
Parsing a fixture and serialising it back must reproduce the file's JSON exactly, which is what catches a field the type forgot or invented.
A new fixture whose name matches no rule fails the suite rather than being skipped.

## Protocol changelog

### Version 1

Initial version, introduced with the daemon in #0120.

Newline-framed JSON-RPC 2.0 over a Unix-domain socket, with a 1 MiB request frame cap inclusive of the terminator.
The `initialize` handshake with the client, protocol, capabilities, and identity params, and the daemon, protocol, instance, capabilities, platform, lifecycle, and config-status result.
The `daemon.status` and `daemon.stop` methods, reachable before the handshake, and the `not_initialized` gate on everything else.
The error table above, codes `-32000` to `-32009`.
The `state.event` and `state.resync_required` notifications, and the `{instance_id, revision, kind, payload}` event envelope.
The read-only methods `account.list` and `message.list`, both behind the handshake, with the account states `ready` and `blocked`, `null` as the spelling of an unlimited `message.list`, and `-32602` for a mailbox the account does not have.
A `message.list` row carries both dates, the derived `date_sort` and the stored `date_display`, so a listing renders from the wire alone.
A `client.type` outside `cli`, `tui` and `gui` is `-32602`, because a method that branches on the caller may not be handed a fourth kind.
A response above the 16 MiB response cap is a `frame_too_large` error carrying `{limit, seen}`, the same pair an oversized request earns.
