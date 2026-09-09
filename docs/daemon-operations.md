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

A `daemon.pid` naming a dead or recycled process is harmless, because nothing locks on it.
It is read for exactly one purpose, the `SIGTERM` fallback in `stop`, and only after the socket has already answered a `daemon.status`, which proves a daemon is there to signal.
A `daemon.json` left behind by a crash is overwritten by the next start.

An unsafe socket is the case that needs a human.
`mp daemon run` refuses to bind and names the reason, `mp daemon start` refuses to remove it, and neither will guess.
The fix is to look at what is at `<data_dir>/runtime/daemon.sock`, move it aside, and start again; the message says so and reports the mode or the owning uid it found.
A socket at the wrong mode is the interesting instance, because it means another user may be able to reach the daemon.

A daemon that answers `daemon.status` but not `daemon.stop` is wedged, and `stop` already falls back to `SIGTERM`.
When that also fails, the command exits nonzero naming the daemon log rather than escalating to `SIGKILL` on its own.

## Test-only environment hooks

Three environment variables exist for the contract tests and for the migration.
None of them has a flag, and none appears in `mp --help`.

`MAILYPOPPINS_DAEMON_FAIL_START=1` makes `mp daemon run` exit nonzero after logging is initialised and before the socket is bound, so `mp daemon start` has a deterministic dead child to report.
The hook sits at that exact point on purpose: a forced failure leaves nothing on disk to clean up.
It is what pins the exit-4 diagnostic in `tests/daemon_lifecycle.rs`.

`MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES=1` opts into account runtimes before Phase 5.
Absent, the daemon creates no runtime and takes no engine lock, and `daemon.status` reports an empty account list.
Present, it reports every configured account as `opening` and leaves it there, since nothing opens a store or takes a lock before Phase 5 either.
It is an environment variable rather than a flag so it cannot leak into `mp --help` or into anyone's muscle memory.

`MAILYPOPPINS_DAEMON_START_LOCK_HELD=1` is the internal handshake between `mp daemon start` and the `mp daemon run` it spawns: the parent holds the start lock, so the child must not block on it.
No user sets this one.

All three read as set for any value other than empty, `0` or `false`.
A test that runs `mp` as a subprocess should `env_remove` the first two, or an exported hook in the developer's shell will change what the test observes.

## Login mode

Not implemented.
A lifecycle command that installs and removes a user-level service, a systemd user unit on Linux and a launchd user agent on macOS, both running `mp daemon run` in foreground mode, is P6-U5 and P6-U6 of the plan.
Login mode changes no socket, no protocol and no client behaviour when it lands; it only changes who runs `mp daemon run`.
The macOS half cannot be smoke-tested on the machine this project is developed on, so the plan already carries the live launchd check as an escalation.
