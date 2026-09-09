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
The response direction carries its own cap, which is why `Decoder::new` takes a limit instead of reading the constant.

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
- `capabilities`, the identifiers the daemon offers.
- `platform`, the host and transport facts a client cannot infer.
- `lifecycle`, the shutdown and restart behaviour of this instance.
- `config_status`, the current state of the configuration on disk.

Compatibility is decided by the declared ranges and the required capabilities.
Disjoint ranges give `protocol_incompatible`, a required capability the daemon does not offer gives `capability_missing`, and a differing directory pair gives `identity_mismatch`.
An application-version difference alone is diagnostic and does not refuse the connection.
Any domain method issued before a successful `initialize` gives `not_initialized`.

## Method families

Methods are domain operations, and each one declares whether it is a query, a command, a long-running operation, or a client-side integration request.

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
The error table above, codes `-32000` to `-32009`.
The `state.event` and `state.resync_required` notifications, and the `{instance_id, revision, kind, payload}` event envelope.
