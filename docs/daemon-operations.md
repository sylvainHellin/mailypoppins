# Daemon operations

How to start, inspect, stop and recover the local daemon, and what it leaves on disk while it runs.
The wire contract is [daemon-protocol.md](daemon-protocol.md); this file is the operator's half of it.

Everything here ships in every build: P4-U1 removed the `daemon` cargo feature, so `cargo install --path .` and a plain `cargo build` both contain the daemon and `--features daemon` no longer exists.
The subcommands are still hidden from `mp --help`, because `tests/cli_help_snapshot.rs` pins the help surface byte-identical to `docs/baselines/pre-daemon/cli-help.txt` and a later unit of the migration moves it deliberately (`.agents/workflow/native-gui-daemon/plan.md`).
`mp daemon --help` and `mp account --help` still render; a hidden command is absent from the parent's `Commands:` block, not from the binary.

## The five lifecycle commands

`mp daemon run` is foreground mode and the only one that becomes a daemon.
It never connects to another daemon.
The startup order is load-bearing: logging is already up from `main`, then the start lock, then the `MIG-04` legacy config-directory move, then the config load, then `<data_dir>/runtime` at mode 0700, then the socket probe, then the bind, then `daemon.pid` and `daemon.json`, then serve.
`--foreground-logs` echoes the lifecycle lines to stderr as well as to the log directory.
A missing `config.toml` is not a startup failure: the daemon serves zero accounts and reports `config_status.state` as `absent` in the handshake until one is written, and an unparseable one reports `invalid` and the first problem.

`mp daemon start` spawns a detached `mp daemon run` and returns once it answers.
It takes the start lock and holds it across both the spawn and the readiness wait, because releasing it at spawn time would let a second starter see a socket inode that no daemon has bound yet and spawn a second daemon.
The child is told the lock is already held through `MAILYPOPPINS_DAEMON_START_LOCK_HELD=1` and skips the acquisition.
A starter that finds a daemon already answering returns 0 without spawning; one that loses the lock race waits for the winner's daemon instead of starting its own.
Readiness is a real `daemon.status` round trip over the socket rather than the existence of a file, polled every 25 ms until `--timeout-secs` (default 10).
The child is spawned through `setsid` with its stdio in the daemon log, so closing the launching terminal does not take it down.

`mp daemon status` reports whether a daemon answers against this data directory.
It prints the instance id, the application version, the protocol range, the pid, the start time and both directories, plus one line per account the daemon reports.
`--json` prints the same fields as one object with a leading `"running"`, and the key set never changes with the answer, so a client branches on `running` alone rather than on which fields exist.
The round trip carries no `initialize`, which is what lets `status` describe a daemon whose protocol range this build cannot negotiate.

`mp daemon stop` asks the running daemon to shut down and waits until it is really gone, up to `--timeout-secs` (default 10).
When the socket answered a moment ago but the `daemon.stop` call does not, the daemon is wedged rather than absent, so the command falls back to `SIGTERM` on the pid in `daemon.json`.
With nothing running it exits 0, and it sweeps a stale socket on the way out, because a stop is exactly when a user expects that tidying.

`mp daemon restart` stops whatever runs, waits for the old pid to disappear (up to 10 s), and then starts this executable's daemon.
The wait is not decoration: a new daemon binding before the old one's cleanup runs would have its own socket unlinked by its predecessor.

## On-demand start, and the no-daemon list

A normal `mp` run that needs the daemon and finds nothing listening starts one itself.
It goes through the same routine `mp daemon start` uses, so the start lock that makes two racing clients produce one daemon, the sweep of a socket a crash left behind, the `current_exe()` spawn into its own session with its stdio in the daemon log, and the early failure on a child that died are all the same code.
The daemon it produces is detached, so it outlives the client that wanted it, which is the whole point.

The bound on the whole sequence is 5 s: the readiness wait first, then a connect retry that backs off from 25 ms to 400 ms until the same deadline.
One budget rather than two, so the worst case a user waits is that number and not some multiple of it.
When it expires, or when the spawn dies, the run exits 4 naming the socket, the daemon log and the literal `mp daemon run` to type by hand.
There is no fallback to answering in process: a command that quietly did the work locally would let the user believe the daemon did it.

Five commands never reach any of that, because each of them either *is* the lifecycle surface or answers from a compiled-in structure:

| command | why |
|---|---|
| `mp daemon *` | `mp daemon run` is the daemon; a `status` that auto-started one would always report one running |
| `mp dump-keys` | the TUI key table is compiled in |
| `mp --help` | clap answers it and exits |
| `mp --version` | the same |
| `mp config path` | it prints where the file would be, and reads nothing |

`mp config init` and `mp config add-account` are deliberately **not** on that list.
The daemon owns configuration and secrets, so an init that wrote `config.toml` behind a running daemon's back would leave it describing a file that changed underneath it.

The policy is one pure function, `mailypoppins::daemon::client::needs_daemon(command, subcommand)`, taking the names as they are typed, so the list can be asserted without running anything.
The single door to the daemon is `mailypoppins::daemon::client::client_session()`; from P4-U4 on every migrated command goes through it and nothing else opens a connection.

## What routes today

Phase 4 is complete: every command a slice contracted routes, and P4-U15 deleted the direct engine path each one used, so there is no in-process fallback left to reach.
The table is what routes and through what:

| command | methods | slice |
|---|---|---|
| `mp show [--json] [--mailbox]` | `message.get` | P4-U4 |
| `mp list-messages [--mailbox] [-n]` | `message.list` | P4-U4 |
| `mp dump-mailbox --json [--mailbox ...]` | `message.list` with `projection: "envelope"` | P4-U4 |
| `mp search --local` | `message.search` | P4-U4 |
| `mp new <name>` | `draft.create` | P4-U6 |
| `mp list [--status]` | `draft.list` | P4-U6 |
| `mp validate [selector]` | `draft.validate` | P4-U6 |
| `mp mark-approved`, `mp mark-draft` | `draft.path`, then `draft.approve` / `draft.demote` | P4-U6 |
| `mp path <selector>` | `draft.path` | P4-U6 |
| `mp edit <selector>` | `draft.path`, then `$EDITOR` in the client | P4-U6 |
| `mp reply [--all] [--mailbox]` | `draft.reply` | P4-U6 |
| `mp forward [--mailbox]` | `draft.forward` | P4-U6 |
| `mp <selector>` (the dry run) | `draft.preview` | P4-U6 |
| `mp archive <selector> [--mailbox]` | `message.archive` | P4-U8 |
| `mp delete <selector> [--mailbox] [--force]` | `message.delete`, or `draft.discard` for a drafts selector | P4-U8 |
| `mp delete --sent` | `draft.discard` with `sent: true` | P4-U8 |
| `mp open <selector> [--mailbox]` | `message.get`, then `message.materialise_attachment` per part, opened in the client | P4-U8 |
| `mp save <selector> [-o dir] [--mailbox]` | `message.get`, then `message.materialise_attachment` and `message.release_handle` per part, written in the client | P4-U8 |
| `mp sync [-n] [--mailbox ...] [--dry-run] [--all-accounts]` | one `sync.quick` per account, in configuration order | P4-U10 |
| `mp fetch [--from ...] [-n] [--mailbox] [--full]` | `message.list_server` | P4-U10 |
| `mp list-mailboxes` | `mailbox.list_server` | P4-U10 |
| `mp watch [--mailbox] [--timeout N]` | `sync.watch`, with the wait and the timeout in the client | P4-U10 |
| `mp send <selector> [-y]` | `draft.preview` for the echo and the preview, then `send.draft` | P4-U12 |
| `mp send --invite …` | `send.invite`, with the preview, the `UID` and the prompt in the client | P4-U12 |
| `mp send-approved [-y] [--all-accounts]` | `draft.list` for the batch listing, then one `send.approved` per account, in configuration order | P4-U12 |
| `mp outbox list` | `send.outbox_list` | P4-U12 |
| `mp outbox retry <id>` | `send.outbox_list`, then `send.outbox_retry` | P4-U12 |
| `mp outbox discard <id>` | `send.outbox_list`, then `send.outbox_discard` | P4-U12 |
| `mp contacts search [query] [-n] [--parsable] [--account]` | `contact.search` | P4-U14 |
| `mp contacts stats [--account]` | `contact.stats` | P4-U14 |
| `mp contacts rebuild [--account]` | one `contact.rebuild` per account, in configuration order | P4-U14 |
| `mp calendar rebuild [--account]` | one `calendar.rebuild` per account, in configuration order | P4-U14 |
| `mp invite accept\|tentative\|decline <selector> [--mailbox]` | `calendar.rsvp` | P4-U14 |
| `mp store gc [--dry-run] [--force] [--all-accounts]` | one `diagnostic.store_gc` per account | P4-U14 |
| `mp cutover [--account] [--dry-run]` | one `config.cutover` per account | P4-U14 |
| `mp config show` | `config.get` | P4-U14 |
| `mp config init`, `mp config add-account` | `config.get` before the first prompt, `config.reload` after the wizard writes | P4-U14 |
| `mp config set-password <smtp\|imap> [--account]` | `config.set_password`, with the `dialoguer` prompt in the client | P4-U14 |
| `mp config oauth2-login [--account]` | `config.oauth2_login`, with the device-code block rendered in the client | P4-U14 |
| `mp config reset-secrets` | `config.get`, then `config.reset_secrets` and one `config.set_password` per re-entered credential | P4-U14 |
| `mp sync`'s post-sync retention sweep | `diagnostic.store_gc`, on the connection the sync already follows | P4-U15 |
| `mp account list` | `account.list`, behind `--daemon` | P2-U11 |
| `mp` (the TUI) | `state.bootstrap`, once at startup, on a session that stays open for the run | P5-U2 |
| `mp`'s mailbox list, sidebar counts and preview body | `message.list` / `draft.list` per mailbox open, `mailbox.list` per recount, `message.get` per cursor move | P5-U4 |

The TUI's row is a session rather than a call: `mp` with no arguments connects through the same `client_session` every command above goes through, before it takes over the terminal, and holds the connection until the user quits.
It paints its shell first and applies the snapshot when it lands, so a slow daemon costs a beat of zeroed counts rather than a blank terminal.
A daemon it cannot reach ends the run with the ordinary exit-4 diagnostic, on a terminal that is still in its normal mode.

Since P5-U4 the three reads a frame needs go the same way, through `crate::tui::queries` and the session the `App` holds.
The two that can wait keep the thread they always had and block on a call rather than on a store open: the mailbox load of `Action::LoadMailbox` and the per-account count of the two-phase startup, both through a `Session::handle()` a worker thread can own, so nothing about the load moved onto the draw thread.
The preview body is the one synchronous read, one `message.get` per cursor move behind the memo that already made a frame on an unchanged selection cost nothing, which is the number `docs/plans/preview-latency.md` budgets.
The listing is transferred whole, once per mailbox open, as `docs/baselines/decisions/list-transfer.md` decided; the row deltas that keep it current decode here already and are applied by nothing until P5-U8 drains the event stream.
An `App` with no session at all falls back to the store-backed readers of `src/tui/app/store_rows.rs`, which is the shape every TUI unit test runs in and, in a real run, only a `Session::connect` that wedged.

`mp config path` is the one domain command that never contacts a daemon: it computes a path and reads nothing, so it is on `needs_daemon`'s no-daemon list for good and is the `UNMIGRATED` control row of `tests/daemon_parity_harness.rs`.

What still answers in process, and why, is four groups, seventeen rows of `CLI_ENGINE_RESIDUE` in `tests/architecture_boundaries.rs` between them, each spelled out in `docs/baselines/phase4-gate-evidence.md`:

- **`mp search` without `--local`.** The server leg (`docs/parity-matrix.md` LST-06) is `not started`: the read slice contracted `mp search --local` and nothing else, so a server search still opens its own IMAP session or Graph client, and the plain-IMAP `has:attachment` post-filter still reads the local index. It is the one command surface Phase 4 leaves on the direct path.
- **The startup preamble.** `init_secrets_backend` and the `SmtpConfig::load` under it print the `⚠ Could not load SMTP config: …` pair and the undecryptable-store exit that every `mp` invocation has printed since long before the daemon. They run before any socket and on the no-daemon list too, so routing them would move bytes on a command that must never need a daemon. `mp send-approved` prints the same line per account for the same reason.
- **`mp config show`'s secret column, token line and signature listing.** `config.get` is contracted *not* to look a secret up (a redacted read that probes the backend for every account on every call is the pattern the contract exists to prevent), so the `(not set)` / `****` column and `token = valid|expired|invalid|not cached` are read locally.
- **The two `config.toml` wizards.** `mp config init` and `mp config add-account` ask `config.get` where the configuration is and what accounts it has, then prompt, test the connection the user just described, prompt again on the result, and write the file themselves before calling `config.reload`. That is one interactive transaction, and splitting it needs a wizard protocol no unit of Phase 4 contracted. They are also the one place a client still writes a secret itself: `secrets::set_secret` for the password they prompted for (`src/config_cmd/init.rs:227,286,633,673`), rather than the `config.set_password` the daemon serves.
A routed command produces the pre-daemon binary's bytes, refusals included: `tests/daemon_read_slice.rs` compares stdout, stderr and the exit code against `~/.cache/mp-oracle/pre-daemon/mp` over one seeded root, for every flag combination and every error case.
That is why a refusal the daemon spelled out comes back typed rather than printed at the call site: `account_not_ready` becomes the sentence a store-less read has always produced, and the rest leaves through `main`'s ordinary error path, which is where the pre-daemon binary reported it.
`tests/daemon_draft_slice.rs` is the same gate for the draft slice, over a fixture whose drafts directories are stashed and restored between the two binaries, because half of those commands write.
`tests/daemon_admin_slice.rs` is the gate for the admin slice, and it masks nothing: the two values that would have forced a mask are removed at the fixture instead, the contact index's `built_at` by building the cache once before either binary runs, and an RSVP's `Message-ID` by keeping every row that actually sends one on the routed side.
`tests/daemon_mutation_slice.rs` is the gate for `mp archive`, `mp delete`, `mp open` and `mp save`, with one row deliberately not byte-identical: `mp open` prints the path it handed the opener, and a daemon-materialised file lives under `<data_dir>/runtime/handles/<handle>/` rather than in the client's own temp directory, so that row is compared with the two directories masked.
`mp save` is the mirror image of it: the absolute destination is what crosses the socket, and the spelling the user typed is what the `✓` lines print.
The direct engine paths those commands used are gone (P4-U15).
`tests/daemon_send_slice.rs` is the gate for the send slice, over a fixture that adds a Graph account, an SMTP account with no credentials and a seeded outbox in every state the listing renders.
Its successful sends are routed-side assertions rather than parity rows, because they run through `MAILYPOPPINS_DAEMON_FAKE_TRANSPORT` and the pre-daemon oracle has no such hook.

`tests/daemon_sync_slice.rs` is the gate for the sync/watch slice, with one sanctioned deviation: `mp watch --mailbox` naming anything but INBOX prints one extra line on stderr saying so, and that line is masked in that row alone.

`mp sync` is the one routed command that follows an operation rather than a call. It bootstraps first, because `operation.progress` and `operation.finished` reach bootstrapped connections only, and it renders every line it prints from those payloads through `mp_client::format`: the drain reports from the progress events, the summary from the `sync.completed` payload the result carries. There is no client-side budget on the wait, exactly as there was no budget on the in-process pass.

The daemon opens the secrets backend on first use, which from P4-U8 includes `message.archive` and `message.delete` and from P4-U10 the whole sync slice: they load the account's IMAP or Graph credentials, and an account with none refuses with the `mp config set-password` sentence *before* the store is touched, so the row stays where it was.

## Exit codes

`0` is success, and for `status` it means a daemon answered.

`1` is a generic failure, and for `status` it means no daemon is running.
A routed command also exits 1 when the daemon refuses the call for a reason it spelled out, an unknown account or a mailbox the account does not have, since that is an ordinary command failure that happens to have travelled over a socket.

`3` is reserved for an incompatible daemon and prints the `mp daemon restart` command.
No Phase 2 path emits it: the lifecycle commands negotiate no protocol version, and a routed command that fails the handshake reports the reason and exits 4.
The code is pinned here and in the protocol document so the unit that adds the client-side version check does not have to invent one.

`4` is a daemon that is unavailable or failed to start.
`mp daemon start` exits 4 when the child dies or never becomes ready, printing the daemon log path, the newest structured log and the `mp daemon run` command to reproduce the failure in the foreground.
A client command exits 4 when it cannot connect, cannot handshake, cannot start a daemon within the bound, or loses the daemon mid-call, and it never falls back to answering in process.
Its message carries the socket path, the daemon log path and the `mp daemon run` command, so the user has something to type rather than only a number.

`2` is not available to the daemon; `mp watch --timeout` already owns it.

## Runtime files

Everything the daemon needs on disk lives in one directory below the data root, so pointing `MAILYPOPPINS_DATA_DIR` at a tempdir moves a whole daemon instance with it.

```
<data_dir>/runtime/                    dir mode 0700
<data_dir>/runtime/daemon.sock         socket mode 0600
<data_dir>/runtime/daemon.start.lock   flock target, never unlinked
<data_dir>/runtime/daemon.pid          diagnostic only, never the lock
<data_dir>/runtime/daemon.json         instance metadata
<data_dir>/runtime/handles/<h>/<name>  one materialised handle's file, dir mode 0700
```

The directory mode is enforced rather than assumed.
Every start calls `ensure_runtime_dir`, which creates the directory when it is missing and tightens it to 0700 when an older build, a lax umask or a hand-rolled `mkdir` left it group-readable.
The socket is bound under `umask(0177)` and then `chmod`ed to 0600, so there is no window at a looser mode.
`daemon.pid` and `daemon.json` are written 0600 too.

`daemon.json` carries the application version, the protocol range, the instance id, the pid, the RFC 3339 start time, and the canonicalised data and config directories.
It is what a client reads when nothing answers the socket, which is how `mp daemon stop` finds a pid to signal.

### The start lock

`daemon.start.lock` is an advisory `flock` taken non-blocking, and it is the primitive that makes concurrent starts safe.
Its lifetime is the file descriptor's, so the kernel releases it on exit however the process died, which is the guarantee a pid file cannot give: a pid file reader finds a number that may name a dead process or a recycled one.

The lock file is never unlinked.
Two starters that each created a different inode under the same name would both take a lock and both win, which is the one failure mode this design exists to prevent.

`mp daemon run` takes the lock itself and drops it once the socket is bound and the runtime files are written; from there the bound socket is what excludes a second daemon.
A child spawned by `mp daemon start` does not take it, because its parent holds it.

### The socket probe

`probe_socket` classifies a path and changes nothing.
It stats with `symlink_metadata` rather than `metadata`, because following a symlink would point the unlink and the connect at two different questions.

- `Absent`: nothing is there. Bind and serve.
- `Live`: a connection was accepted. Never unlinked.
- `Stale`: our own 0600 socket, owned by this uid, refusing connections. The only classification that may be removed.
- `Unsafe`: anything else, with a reason. Never unlinked and never used.

A path is `Unsafe` when it is not a socket (a regular file, a directory, a symlink), when its owner is not the effective uid, when its mode is anything but 0600 including a stray execute bit, when it cannot be stat'ed, or when the connection attempt fails with something other than a refusal.
The metadata checks run before the connection attempt, so an unsafe path is reported unsafe whether or not something answers on it.
`Unsafe` outranks `Stale`, because a daemon that deletes a file it does not understand is a daemon that deletes user data.

`remove_stale_socket` unlinks only for `Stale`.
Every other classification is an error, `Absent` included: deleting nothing is cheap, but a caller that reached that call has lost track of its own state and should hear about it rather than proceed on a wrong belief.

### Shutdown

A clean shutdown unlinks the socket, and unlinks `daemon.json` and `daemon.pid` only while they still name this instance, so a daemon that started after us does not have its metadata deleted by our exit.
`SIGTERM` and `SIGINT` both run the same path, so a foreground daemon killed with Ctrl-C leaves no socket behind.

## Configuration ownership

The daemon reads `config.toml` once, at startup, and owns it from then on.
What it holds is a `ConfigStore` (`src/daemon/config.rs`): the loaded `GlobalConfig`, the state it is in (`ok`, `absent`, `invalid`), the file it came from, and a revision that starts at 0 and moves by one per successful swap.
Every read method resolves its account against that store rather than against a list captured at startup, so a reload is visible to the next `account.list` without a restart, and `mp daemon status` lists the accounts the live configuration names.

Nothing watches the file yet: a swap happens when a client asks for one, through `config.reload`, `config.init` or `config.add_account`.
The file watcher is a later unit; the mechanism it will drive is this one.

A swap is atomic in the sense that matters: the candidate is parsed and validated in full before anything is stopped, written or installed.
A candidate that does not load leaves the previous snapshot live, its runtimes running and their engine locks held, and the revision where it was; the caller gets `-32007` with `{path, line?, message}` and every bootstrapped client gets the same three fields as a `config.invalid` event.
A candidate that loads is installed, and then the runtimes are reconciled in one order: stop the accounts that went, restart the accounts whose effective configuration changed, start the accounts that appeared, and announce the whole thing with one `config.changed`.

`config.reload` returns only once every runtime it touched has settled, so a removed account's engine lock is free by the time the call answers and an account can be renamed in one edit without the new runtime racing the old one.
A started runtime creates its account directory if it is missing, exactly as `mp config init` does, so an account added by a hand edit comes up the same way as one added through `config.add_account`.
A daemon without `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES=1` swaps the configuration and starts nothing, because it has no runtimes to reconcile.

Secrets go in through `config.set_password` and nowhere else, under the keys the pre-daemon binary already reads (`smtp-password-<account>`, `imap-password-<account>`).
The backend is opened on first use, not at startup: a first run has no configuration to select one from.
It is a process-wide singleton, so a `secrets_backend` change in `config.toml` takes a daemon restart even though the rest of the swap is live.

The wire shapes, the error payloads and the two event kinds are in [daemon-protocol.md](daemon-protocol.md).

## The draft and signature watcher

The daemon polls every `<account_dir>/drafts/*.md` and every `<config_dir>/signatures/*.md` once a second and publishes what settled (`src/daemon/watch.rs`).
It is the TUI's one-second fingerprint poll moved into the daemon, with no `notify` dependency, one `stat` per file per poll and no file contents read while looking.

It runs whether or not `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES=1` is set: it opens no store and takes no engine lock, so gating it behind the runtimes would cost a lock nothing about drafts needs.
The roots are one per configured account plus the global signatures directory, re-derived by every successful `config.reload`, so an added account is watched from the next poll and a removed one is forgotten with its rows.
A root that does not exist is an empty root and is picked up on the poll after it appears: an account that has never had a draft is not a startup failure.

A change settles only after it has held still for 300 ms, and that debounce is what makes an editor's save one event.
A file whose `(mtime, size)` moved becomes pending; a pending file that moves again restarts its window; a pending file that has held still for the debounce is reparsed once, in the state it is in at that moment.
Absence is debounced the same way, so `:w` with `backupcopy=no` - rename the original away, create a new file at the same path - is one `draft.changed` rather than a removal followed by an addition that flickers the row out of every client's list.
A file that appeared and vanished inside one window is never announced, and therefore never withdrawn.

A draft that parses travels as `draft.changed`, one that does not as `draft.invalid` with a diagnostic, and a draft that was announced and is now gone as `state.remove` of `draft:<account>/<id>`.
A signature travels as the lifecycle event `signature.changed`.

The watcher never opens a draft for writing: not to mint an `id:`, not to normalise, not to restore a file it saw disappear.
That is why a draft with no `id:` is announced under its file stem, where the explicit index refresh would mint one: minting inside a watcher means writing to a file an editor is holding open, which is the exact burst the debounce exists to survive.
A file that will not parse is left byte-identical, mtime included, which is also what keeps it from republishing the same diagnostic on every poll.

## Materialised handles

`message.materialise_attachment` and `message.materialise_html` write the bytes a client asked for into `<data_dir>/runtime/handles/<handle>/<name>`, one directory per handle at mode 0700, and answer with the path (`src/daemon/handles.rs`).
The wire shapes are in [daemon-protocol.md](daemon-protocol.md#materialised-handles); what follows is what the files and the lifetime do to a running daemon.

A handle lives ten minutes by default and is released by `message.release_handle` or by expiry, never by a disconnect: a viewer holding an open file must not lose it because the socket that asked for it went away.
While it lives it pins the blobs it read, and the retention sweep skips them, so a sweep running beside a viewer evicts something else or nothing.

**Reaping is lazy.**
Every handle call first drops the entries that expired since the last one and unlinks their directories; there is no periodic tick.
The pin is already false the instant a handle expires - the table answers "is this blob spoken for *now*", not "was the last tick recent enough" - so a reaper would buy nothing a sweep needs, and its interval would be one more thing to get wrong.
What that costs is a daemon nobody calls keeping expired scratch on disk until the next call; it sits inside the 0700 runtime directory, and the next materialisation clears it.
A daemon that exits leaves `handles/` behind, which is why the directory is under `runtime/`: it is scratch, and removing the whole tree is safe with no daemon running.

## Account runtimes

With `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES=1` the daemon starts one `AccountRuntime` per configured account (`src/daemon/runtime/account.rs`), and without it there is no runtime, no engine lock and no store to open.
The starts run off the startup path, on `spawn_blocking`, because each one takes an advisory lock and opens SQLite; the socket is already accepting connections while they happen.

An account is `opening` from the moment the daemon lists it until its start comes back, then `ready` or `blocked`.
That is what `mp daemon status --json` prints and what a bootstrapped client receives as an `account.state_changed` event, and the two cannot disagree: the runtime table is filled before the change is committed.
A start that fails outright, an unusable account directory or an unopenable store, reads as `blocked` with the failure as its reason.

### The engine lock

A ready runtime holds `<account_dir>/store.lock` for its whole lifetime, not for the length of one operation, which is what makes the daemon *the* engine for that account.
While it lives, `mp sync` in a terminal and an open TUI both refuse the ingest and the queue drains and leave them to the daemon (`Sync skipped: another engine is syncing '<account>'`), exactly as a second `mp sync` already did.
The lock is released when the daemon exits, by `close(2)`, so a crash leaves nothing to reap.

A runtime that cannot take the lock, because a `mp sync` or a TUI is holding it, still starts and reports `blocked`.
It opens the store and serves reads from it and runs no engine at all: no tick, no drain, no IMAP session.
This is the read-only degrade `src/engine_lock.rs` describes, and it is why starting a daemon while another client is mid-sync is safe rather than fatal.

### The read pool

Each runtime opens two read connections over the account's store, the size `docs/baselines/decisions/read-pool.md` measured, plus the writer connection a sync uses, which is not one of the two.
A read checks a connection out and returns it on drop; when both are out, the next read waits rather than failing, since exhaustion resolves in microseconds and a retry loop in every caller would not.
Every pooled connection is opened through `Store::open`, so it carries the store's pragmas, `busy_timeout` included, and the first one created the store file if the account had never synced.

A runtime's store therefore exists from the moment it comes up ready, which is a change from Phase 3a: an account nobody has synced now has a `store.sqlite3` as soon as a daemon with the opt-in has started.

### Ticks

A tick is `run_tick_with_drains` (#0114) and nothing else: drain, sync, drain, with the tail drain running after a body that failed, and the outbox before the mutation queue in each drain.
The body's error is carried on the outcome rather than propagated, because the tail has already run by the time anyone hears about it.
A second tick arriving while one runs joins it and reports the running tick's outcome rather than starting a second engine pass.
Each tick carries the account's `imap.body_fetch_deadline_secs` as its per-mailbox body budget, with `0` meaning unbounded; `mp sync`, the explicit recovery path, stays unbounded whatever the config says.

Nothing schedules a tick yet.
The periodic scheduler is Phase 5/6 of the plan, so in this build a runtime holds its lock, serves reads, and ticks only when something in the process asks it to.

## Logs

`mp daemon start` points the detached child's stdout and stderr at `<data_dir>/logs/daemon.log`, opened in append mode, and that is the path the exit-4 diagnostic prints.
It holds whatever the process wrote outside the logging framework, including a panic.

The structured log is the ordinary one, `<data_dir>/logs/mailypoppins-YYYY-MM-DD.log`, and every daemon line there is prefixed `[daemon]`.
Filter a startup with `rg '\[daemon\]' <data_dir>/logs/mailypoppins-*.log`.
The start-failure diagnostic names both files, since the reason is usually in the second one and the first is where an early crash lands.

## Recovery

A stale socket after a crash needs no intervention.
`mp daemon start` probes the path while it holds the start lock and removes the socket only on `Stale`, and `mp daemon run` does the same before it binds.
`mp daemon stop` sweeps one too when nothing answers.

A `daemon.pid` naming a dead or recycled process is harmless, because nothing locks on it and nothing signals it.
It is read for exactly one purpose, `cleanup` at shutdown, which unlinks it only while it still names the exiting instance.
The pid that gets signalled comes from `daemon.json`: the `SIGTERM` fallback in `stop` uses it, and only after the socket has already answered a `daemon.status`, which proves a daemon is there to signal, and `restart` polls it to watch the previous instance go away.
A `daemon.json` left behind by a crash is overwritten by the next start.

An unsafe socket is the case that needs a human.
`mp daemon run` refuses to bind and names the reason, `mp daemon start` refuses to remove it, and neither will guess.
The fix is to look at what is at `<data_dir>/runtime/daemon.sock`, move it aside, and start again; the message says so and reports the mode or the owning uid it found.
A socket at the wrong mode is the interesting instance, because it means another user may be able to reach the daemon.

A daemon that answers `daemon.status` but not `daemon.stop` is wedged, and `stop` already falls back to `SIGTERM`.
When that also fails, the command exits nonzero naming the daemon log rather than escalating to `SIGKILL` on its own.

## Test-only environment hooks

Thirteen environment variables exist for the contract tests and for the migration.
None of them has a flag, and none appears in `mp --help`.

`MAILYPOPPINS_DAEMON_AUTOSTART=0` turns on-demand starting off, leaving the exit-4 diagnostic in its place.
It is not test-only either: it is what an operator debugging a daemon that dies as fast as it is started wants, and what a test that has to observe "nothing is listening, and nothing appeared" needs.
`0`, `false`, `no` and an empty value disable it; anything else, and being unset, leaves it on.
Its name is `mailypoppins::daemon::client::AUTOSTART_ENV`.

`MAILYPOPPINS_DAEMON_AUTOSTART_TIMEOUT_MS=<n>` sets the whole auto-start budget, readiness wait and connect retry together, which defaults to 5000 ms.
Unset, unparseable or zero means the default, as with the daemon's other numeric hooks: a client may not refuse to run over an environment variable it did not understand.
It exists because a test of "it gives up after the bound" would otherwise cost five seconds each time; `tests/daemon_autostart.rs` runs its failure cases at 800 ms.
Its name is `mailypoppins::daemon::client::AUTOSTART_TIMEOUT_ENV`.

`MAILYPOPPINS_DAEMON_REQUIRE=1` makes a command that answered from its own process exit 1 saying so, instead of producing output a parity comparison would credit to the daemon.
The check sits at the single client entry point, so it cannot drift away from what actually routes: `client_session` records that it handed out a connection, and the end of the run refuses to be silent when nothing did.
A command on the no-daemon list fails immediately with a different message, because asking one of those to prove it routed is a mistake in the test rather than a failure of the command.
It cannot catch a run that leaves through `std::process::exit` or through an error, neither of which returns to the check; every migrated command returns normally, and a command that failed proves nothing about routing anyway.
Its name is `mailypoppins::daemon::client::REQUIRE_ENV`, and `DaemonFixture::mp_routed` in the parity harness is what sets it.

`MAILYPOPPINS_DAEMON_FAIL_START=1` makes `mp daemon run` exit nonzero after logging is initialised and before the socket is bound, so `mp daemon start` has a deterministic dead child to report.
The hook sits at that exact point on purpose: a forced failure leaves nothing on disk to clean up.
It is what pins the exit-4 diagnostic in `tests/daemon_lifecycle.rs`.

`MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES=1` opts into account runtimes before Phase 5, and it is the one hook here that is not test-only: it turns on real behaviour, described under [Account runtimes](#account-runtimes) above.
Absent, the daemon creates no runtime and takes no engine lock, and `daemon.status` reports an empty account list.
What that lock covers grew in #0122: besides the outbox and mutation-queue drains it now guards the IMAP sync ingest, so once a runtime holds it for its lifetime a concurrent `mp sync` prints `Sync skipped: another engine is syncing '<account>'; leaving the ingest to it` and exits 0 instead of ingesting the same window twice.
It is an environment variable rather than a flag so it cannot leak into `mp --help` or into anyone's muscle memory.

`MAILYPOPPINS_DAEMON_FAKE_READY_AFTER_MS=<n>` flips every configured account to `ready` `n` milliseconds after the **first** `state.bootstrap`, committing one change per account in `config.toml` order.
Phase 3a starts no account runtimes, so nothing else would ever report readiness and the snapshot would never converge by event.
The countdown starts at the bootstrap rather than at daemon startup, so a client that bootstraps cannot lose the race against it and no test has to sleep to win it.
It is what pins the readiness event in `tests/daemon_bootstrap.rs`, and its name is `mailypoppins::daemon::state::FAKE_READY_ENV` so the test and the daemon cannot drift apart.

`MAILYPOPPINS_DAEMON_FAKE_EVENT_BURST=<n>` commits `n` mailbox-count changes against the first configured account after **every** `state.bootstrap`, with mailbox slugs `burst-<i>` from a counter that starts at `0` and never restarts for the life of the process.
Phase 3a commits no change on its own, so nothing would otherwise fill a connection's outbound queue and the coalescing, cap and resync paths would be untestable.
The slugs are distinct on purpose, so no two of the changes coalesce, and they name mailboxes no account has, so a burst fills queues without moving the snapshot a second client takes.
The changes are committed off the bootstrap's own path and after its revision was captured, so no bootstrap's latency includes them and every burst revision is above the one it reported.
With no configured account there is nothing to commit against and the hook does nothing.
It is what pins the backpressure cases in `tests/daemon_events.rs`, and its name is `mailypoppins::daemon::state::events::FAKE_EVENT_BURST_ENV`.

`MAILYPOPPINS_DAEMON_FAKE_SYNC_OUTCOME=<json>` commits one `sync.completed` outcome per element of a JSON array after **every** `state.bootstrap`, in array order, against the first configured account.
A bare object is read as an array of one.
Phase 3b schedules no tick, so nothing would otherwise publish an outcome and every assertion about one arriving over the socket would be vacuous.
Each element's `account` is ignored and replaced by the configured name, and every other field travels verbatim, `severity` included, so a test can pin a severity no fake sync could produce.
The outcomes are committed off the bootstrap's own path and after its revision was captured, so every one of them lands above the revision the bootstrap reported.
It is inert unless `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES=1` is set too: without a runtime there is no tick, and a hook that fired anyway would report on an engine that is not running.
Unset, empty, unparseable, or with no configured account it does nothing.
It is what pins the socket cases in `tests/daemon_sync_outcome.rs`, and its name is `mailypoppins::daemon::sync_outcome::FAKE_SYNC_OUTCOME_ENV`.

`MAILYPOPPINS_DAEMON_FAKE_TRANSPORT=<json>` serves the SMTP submission and the Sent-mailbox APPEND in process and writes one line per transport event to a log file.
`src/send.rs`'s transport is TLS-only on both of its branches, so a plaintext `TcpListener` cannot serve it and a TLS one would need a certificate generator this tree does not depend on; without the hook every success path of the send slice would be unpinned.
Its value is a JSON object, every key optional: `log` is the ledger path, `reject` maps a recipient address to the reason the server refuses it for good, `append` is `ok`, `fail` or `swallow_ack`, and `append_delay_ms` parks inside the APPEND.
`swallow_ack` files the copy and reports a failure, which is the ambiguity the Message-ID dedup search exists for; the search answers out of the ledger, so a copy an earlier attempt filed is found by a later one.
Each line is `submit <message-id> <address> accepted|rejected`, `append <mailbox> <message-id>` or `search <mailbox> <message-id>`, appended with one `O_APPEND` write so two concurrent drains cannot lose a line.
An account with no credentials sends through it all the same: the fake *is* the transport, so a `SmtpConfig::load` that fails is replaced by a placeholder while the hook is armed.
It is read by whichever process submits, which after P4-U12 is the daemon and only the daemon, so the pre-daemon oracle cannot see it and no row that uses it is a parity row.
Unset, empty or unparseable it does nothing.
It is what pins the successful sends in `tests/daemon_send_slice.rs`, and its name is `mailypoppins::daemon::methods::send::FAKE_TRANSPORT_ENV`.

`MAILYPOPPINS_DAEMON_FAKE_OPERATIONS=1` registers one extra method, `test.operation`, so a client can start, watch and cancel a long-running operation.
Its params are `{"steps": u64, "step_ms": u64, "scope": "durable"|"client_scoped", "fail_at": u64|null}`; it answers immediately with `{"operation_id": str}` and reports `steps` times, `step_ms` apart, before succeeding with `{"steps": steps}`.
A non-null `fail_at` of `k` fails with `-32603` and `{"failed_at": k}` after the `k`-th report instead of continuing, and a cancelled operation's worker stops before its next report.
Phase 3a has no real long-running method - sync, auth and the rebuilds all arrive in Phase 5 - so without the hook the whole wire half of the operation contract would be untestable until then.
`scope` is the one parameter a real method would not take: a real one passes its own `MethodSpec`'s `cancel_scope`, and one test method has to cover both halves of the disconnect contract.
The method is registered only when the hook is set, so a daemon nobody armed it on neither serves nor advertises it.
It is what pins the wire cases in `tests/daemon_operations.rs`, and its name is `mailypoppins::daemon::operations::FAKE_OPERATIONS_ENV`.

`MAILYPOPPINS_DAEMON_WATCH_POLL_MS=<n>` and `MAILYPOPPINS_DAEMON_WATCH_DEBOUNCE_MS=<n>` set the watcher's poll interval and its debounce, which default to 1000 ms and 300 ms.
Unset, unparseable or zero means the default, as with the readiness hook.
Both exist because a test that waited for a real poll plus a real debounce would cost 1.3 seconds per assertion and the draft contract makes a lot of them; the sandboxes in `tests/daemon_draft_watch.rs` run at 25 ms and 100 ms.
Their names are `mailypoppins::daemon::watch::WATCH_POLL_ENV` and `WATCH_DEBOUNCE_ENV`, so the test and the daemon cannot drift apart.

`MAILYPOPPINS_DAEMON_HANDLE_TTL_MS=<n>` sets how long a materialised handle lives, which defaults to 600000 ms (ten minutes).
Unset, unparseable or zero means the default, as with the watcher's two hooks: a daemon may not fail to start over a stray variable.
It exists because a test of "an expired handle is no longer releasable" would otherwise cost ten minutes; `tests/daemon_handles.rs` runs its sandboxes at 60000 ms and its one expiry case at 400 ms.
The lifetime is read once, at startup, so a handle cannot be minted under one lifetime and released under another; its name is `mailypoppins::daemon::handles::HANDLE_TTL_ENV`.

`MAILYPOPPINS_DAEMON_START_LOCK_HELD=1` is the internal handshake between `mp daemon start` and the `mp daemon run` it spawns: the parent holds the start lock, so the child must not block on it.
No user sets this one.

The flag-shaped ones read as set for any value other than empty, `0`, `false` or `no`; the readiness and burst hooks read as absent unless their value parses as a number, and the sync-outcome hook unless its value parses as a payload.
`MAILYPOPPINS_DAEMON_AUTOSTART` is the one that reads the other way round, being on by default.
A test that runs `mp` as a subprocess should `env_remove` every hook it does not want, or an exported one in the developer's shell will change what the test observes.

## The parity harness

Every Phase 4 slice that moves a command onto the daemon is gated on byte parity with the pre-daemon binary, and `tests/support/parity.rs` is what makes that comparison (`tests/daemon_parity_harness.rs` tests the harness itself).

`DaemonFixture::start(tmp)` boots `mp daemon run` against `tmp` used as `HOME`, `MAILYPOPPINS_DATA_DIR` and `MAILYPOPPINS_CONFIG_DIR` at once, clears all ten environment hooks above so an exported variable cannot change an outcome, and returns once `<tmp>/runtime/daemon.sock` accepts a connection.
`fixture.mp(args)` runs the client against the same root; `oracle(args, tmp)` runs the pre-daemon binary against **the same** root, so a config path in the output is the same string on both sides and the comparison stays literal instead of normalised.
`stop` kills the daemon and waits for it to be gone, and `Drop` does the same, so a panicking test leaks nothing.

The oracle binary is `$MP_ORACLE_BIN`, else `~/.cache/mp-oracle/pre-daemon/mp`, else a build of the `pre-daemon` tag into that path; the procedure and the cache layout are in [baselines/pre-daemon/README.md](baselines/pre-daemon/README.md#the-oracle-binary).
Build it once by hand before the first suite run, or the run that finds the cache empty pays 75 s for it under a lock every parallel test then waits on.

P4-U2 added the half of the harness that runs with no daemon: `mp_no_daemon(args, root)` runs the client with auto-start off, `mp_autostart(args, root, budget_ms, cwd)` runs it with auto-start on and an explicit bound, and `stop_daemon(root)` ends whatever an auto-start produced through `mp daemon stop`, because a detached daemon is nobody's child and there is no pid to signal that anyone owns.
`DaemonFixture::start_in(root, cwd)` and `fixture.mp_in(cwd, args)` give the daemon and the client different working directories, which is how a slice proves the daemon's cwd carries no meaning: start it from `/`, stand the client in a temp directory, and a relative path still resolves under the temp directory.
`fixture.mp_routed(args)` sets `MAILYPOPPINS_DAEMON_REQUIRE=1`; a slice switches its parity assertion to it the moment the command routes, and the assertion stops being a tautology.

## Path resolution is the client's

The daemon is started from wherever the machine happened to start it, so a relative path that reached it would resolve somewhere nobody chose.
The client is the process with a meaningful working directory, and it absolutises every user-supplied path before that path crosses the socket, through `mailypoppins::daemon::client::absolutise`.
Nothing is canonicalised: a destination that does not exist yet is the normal case, and following symlinks would answer a different question than the user asked.

Two places carry user paths today and both apply the rule from P4-U2 on, before either command routes:

- `mp save -o`, whose default is the current directory. `mp save` with no `-o` therefore prints absolute paths where it used to print `./name`; the files land exactly where they always did.
- A draft's `attachments:` entries are the exception, resolved by `send::resolve_attachment_paths` **inside the daemon** against the draft file's own directory. No attachment path crosses the wire - `send.draft` carries a selector and the daemon reads the draft itself - so there is nothing for the client to absolutise, and the daemon's own cwd is whichever directory happened to start it. A relative entry is therefore anchored to the draft rather than to the sender, which is an accepted divergence from the pre-daemon binary (`docs/parity-matrix.md` ATT-03); the anchor is a parameter of the function, so no library code reads `current_dir()`. The other visible change is that a missing attachment is reported by its full path.

## Login mode

Not implemented.
A lifecycle command that installs and removes a user-level service, a systemd user unit on Linux and a launchd user agent on macOS, both running `mp daemon run` in foreground mode, is P6-U5 and P6-U6 of the plan.
Login mode changes no socket, no protocol and no client behaviour when it lands; it only changes who runs `mp daemon run`.
The macOS half cannot be smoke-tested on the machine this project is developed on, so the plan already carries the live launchd check as an escalation.
