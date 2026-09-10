# Daemon operations

How to start, inspect, stop and recover the local daemon, and what it leaves on disk while it runs.
The wire contract is [daemon-protocol.md](daemon-protocol.md); this file is the operator's half of it.

Everything here is behind the `daemon` cargo feature and hidden from `mp --help` until P4-U1 of the migration (`.agents/workflow/native-gui-daemon/plan.md`).
An `mp` from `cargo install --path .` contains none of it, and the commands below need a build made with `--features daemon`.
The subcommands carry `hide = true` on top of the feature gate because `tests/cli_help_snapshot.rs` holds one snapshot for the featured and the unfeatured build, and a visible subcommand would fail one of the two runs against a snapshot it cannot satisfy.

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

## Exit codes

`0` is success, and for `status` it means a daemon answered.

`1` is a generic failure, and for `status` it means no daemon is running.
A routed `mp --daemon` command also exits 1 when the daemon refuses the call for a reason it spelled out, an unknown account or a mailbox the account does not have, since that is an ordinary command failure that happens to have travelled over a socket.

`3` is reserved for an incompatible daemon and prints the `mp daemon restart` command.
No Phase 2 path emits it: the lifecycle commands negotiate no protocol version, and a routed command that fails the handshake reports the reason and exits 4.
The code is pinned here and in the protocol document so the unit that adds the client-side version check does not have to invent one.

`4` is a daemon that is unavailable or failed to start.
`mp daemon start` exits 4 when the child dies or never becomes ready, printing the daemon log path, the newest structured log and the `mp daemon run` command to reproduce the failure in the foreground.
A routed `mp --daemon` command exits 4 when it cannot connect, cannot handshake, or loses the daemon mid-call, and it never falls back to answering in process: a command that asked for the daemon and quietly ran locally would let the user believe the daemon did the work.

`2` is not available to the daemon; `mp watch --timeout` already owns it.

## Runtime files

Everything the daemon needs on disk lives in one directory below the data root, so pointing `MAILYPOPPINS_DATA_DIR` at a tempdir moves a whole daemon instance with it.

```
<data_dir>/runtime/                    dir mode 0700
<data_dir>/runtime/daemon.sock         socket mode 0600
<data_dir>/runtime/daemon.start.lock   flock target, never unlinked
<data_dir>/runtime/daemon.pid          diagnostic only, never the lock
<data_dir>/runtime/daemon.json         instance metadata
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

Six environment variables exist for the contract tests and for the migration.
None of them has a flag, and none appears in `mp --help`.

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

`MAILYPOPPINS_DAEMON_FAKE_OPERATIONS=1` registers one extra method, `test.operation`, so a client can start, watch and cancel a long-running operation.
Its params are `{"steps": u64, "step_ms": u64, "scope": "durable"|"client_scoped", "fail_at": u64|null}`; it answers immediately with `{"operation_id": str}` and reports `steps` times, `step_ms` apart, before succeeding with `{"steps": steps}`.
A non-null `fail_at` of `k` fails with `-32603` and `{"failed_at": k}` after the `k`-th report instead of continuing, and a cancelled operation's worker stops before its next report.
Phase 3a has no real long-running method - sync, auth and the rebuilds all arrive in Phase 5 - so without the hook the whole wire half of the operation contract would be untestable until then.
`scope` is the one parameter a real method would not take: a real one passes its own `MethodSpec`'s `cancel_scope`, and one test method has to cover both halves of the disconnect contract.
The method is registered only when the hook is set, so a daemon nobody armed it on neither serves nor advertises it.
It is what pins the wire cases in `tests/daemon_operations.rs`, and its name is `mailypoppins::daemon::operations::FAKE_OPERATIONS_ENV`.

`MAILYPOPPINS_DAEMON_START_LOCK_HELD=1` is the internal handshake between `mp daemon start` and the `mp daemon run` it spawns: the parent holds the start lock, so the child must not block on it.
No user sets this one.

The four flag-shaped ones read as set for any value other than empty, `0` or `false`; the readiness and burst hooks read as absent unless their value parses as a number, and the sync-outcome hook unless its value parses as a payload.
A test that runs `mp` as a subprocess should `env_remove` every hook it does not want, or an exported one in the developer's shell will change what the test observes.

## Login mode

Not implemented.
A lifecycle command that installs and removes a user-level service, a systemd user unit on Linux and a launchd user agent on macOS, both running `mp daemon run` in foreground mode, is P6-U5 and P6-U6 of the plan.
Login mode changes no socket, no protocol and no client behaviour when it lands; it only changes who runs `mp daemon run`.
The macOS half cannot be smoke-tested on the machine this project is developed on, so the plan already carries the live launchd check as an escalation.
