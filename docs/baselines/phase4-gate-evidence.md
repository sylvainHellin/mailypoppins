# Phase 4 exit gate

The six gate lines of Phase 4 of the daemon migration (`.agents/workflow/native-gui-daemon/plan.md`, section 4), each mapped to the test or command that proves it, with the result recorded rather than asserted.

Ticket [#0123](../tickets/0123-cli-cutover.md).
Evidence taken on 2026-09-11 at `db2d67d` on branch `daemon`, twenty-three commits past the Phase 3b exit, all of them Phase 4's own.
Toolchain `rustc 1.96.0 (ac68faa20 2026-05-25)`, `cargo 1.96.0 (30a34c682 2026-05-25)`, host Ubuntu 26.04 LTS, Linux 7.0.0-22-generic, AMD Ryzen 7 PRO 8845HS, 28 GiB RAM.

Five of the six pass as written.
The sixth, "every command that touches domain state uses `mp-client`", passes for every command a Phase 4 slice contracted and does not for the one command no slice did: the server leg of `mp search`.
That, and three narrower survivors, are the residue table below.

## The whole-tree run

Every count below is a slice of this one run.

```sh
timeout 1800 cargo test --workspace --offline --no-fail-fast
```

2071 pass, 0 fail, 4 ignored, across 43 result lines.
`pgrep -af 'mp daemon'` afterwards returns nothing.

2071 against P4-U14's 2068: the three additions are this unit's, all in `tests/architecture_boundaries.rs`, which goes from 3 tests to 6.
No pre-existing test changed its assertions, and `git diff --stat 4d8349e..HEAD -- tests/` is that one file.

The six slice suites, each green three times running on its own:

| suite | tests |
|---|---:|
| `daemon_read_slice` | 22 |
| `daemon_draft_slice` | 34 |
| `daemon_mutation_slice` | 35 |
| `daemon_sync_slice` | 38 |
| `daemon_send_slice` | 50 |
| `daemon_admin_slice` | 44 |

## Every command that touches domain state uses `mp-client`

Units P4-U3 to P4-U15.

```sh
cargo test --offline --test architecture_boundaries
```

6 passed, 0 failed.

P4-U15 deleted every migrated command's direct engine path: thirteen `#[allow(dead_code)]` items in `src/main.rs` that named this unit, `src/config_cmd/oauth2.rs` and `src/config_cmd/reset.rs` whole, `config_cmd::password::cmd_set_password`, `contacts_cmd::{handle_rebuild, handle_stats}`, `calendar_cmd::handle_rebuild` and `cutover::handle_cutover`.
1 029 lines went, 64 came back, and `cargo build --offline` reports zero warnings, dead-code warnings included.

The post-sync retention sweep moved with them, from `store::sweep::sweep` in the client to `diagnostic.store_gc` on the connection `mp sync` already follows.
It was the last store `mp sync` opened.

### The residue, and why each entry is there

`CLI_ENGINE_RESIDUE` in `tests/architecture_boundaries.rs` is the allow-list, seventeen `(file, symbol, reason)` rows in four groups.
The gate asked for zero; four causes stand between here and zero, and none of them can be removed without editing a T unit's test file, which an implementer may not do.

**(a) The server leg of `mp search`** - 6 rows in `src/main.rs` (`GraphClient::`, `GraphConfig::load`, `ImapConfig::load`, `imap_client::`, `Store::open`).
`docs/parity-matrix.md` LST-06 is `not started`: the read slice (P4-U3/U4) contracted `mp search --local` and nothing else, and no `message.search_server` exists.
`MESSAGE_READ_METHOD_SPECS` is pinned at three by `tests/daemon_read_slice.rs` and `MESSAGE_SERVER_METHOD_SPECS` at one by `tests/daemon_sync_slice.rs`, so the method the leg needs cannot be added in this unit.
The `Store::open` in the group is the plain-IMAP `has:attachment` post-filter, which reads the local index to decide which server hits carry an attachment.

**(b) The startup preamble** - 3 rows in `src/main.rs` (`SmtpConfig::load`, `init_secrets_backend(`, `secrets_path(`).
`init_secrets_backend` and the `SmtpConfig::load` under it print, between them, the `⚠ Could not load SMTP config: …` pair and the `Undecryptable` exit that every `mp` invocation has printed since long before the daemon.
They run before any socket, and they run on the no-daemon list too (`mp config path`, `mp daemon *`), so routing them would either need a daemon for a command that must never need one or move bytes on it.
`mp send-approved`'s per-account load is the same line for the same reason, one round trip too early to come off `send.approved`.
`secrets_path()` is a path and reads nothing; it is listed because the scan looks for the module, not because it opens anything.

**(c) `mp config show`'s three local probes** - 3 rows in `src/config_cmd/show.rs` (`get_secret(`, `load_token_cache(`, `token_cache_path(`).
`tests/daemon_config.rs` contracts `config.get` **not** to look a secret up, in as many words, and pins its result at four top-level keys; `tests/daemon_admin_slice.rs` pins the config family at nine methods with a compile-time assertion.
So neither a field on `config.get` nor a tenth `config.*` method can carry the `(not set)` / `****` column and the token-cache line, and the alternative - a second family - is a contract change, not an implementation.

**(d) The two `config.toml` wizards** - 5 rows in `src/config_cmd/{helpers,init}.rs` (`SmtpTransport::`, `imap_client::` twice, `GraphClient::`, `device_code_flow(`, `set_secret(`).
`mp config init` and `mp config add-account` ask the daemon where the configuration is and what accounts it has, then prompt, test the connection the user just described, prompt again on the result, and write.
That is one interactive transaction whose intermediate results steer the next prompt; splitting it needs a wizard protocol, which no unit of Phase 4 contracted.

The TUI is **out of scope of this list until Phase 5** (#0124), which is the unit that takes it off the direct path.
Its own residue is the first half of the same file, `tests/fixtures/tui-engine-imports.txt`, unchanged by this unit.

## No such command opens stores, secrets, network backends, or engine locks

Unit P4-U15, the same test.
The static half passes as described above.

The runtime half the plan names - "a runtime assertion that the client process holds no `store.lock`" - is **not taken as a dedicated assertion**.
What exists is `tests/engine_lock_ingest_cli.rs`, which holds an account's engine lock in the test process and asserts the `mp sync` it forks prints its skip line and exits 0; that proves the client does not take the lock from a process that would have to fight for it, which is the property the gate line is after, from the other side.
No CLI handler names `EngineLock` at all, which the scan checks and which is the stronger static statement.

## Cold and warm one-shot CLI latency are within the Phase 0 baselines

Unit P4-U15.

### Method

`hyperfine` is not installed on this host, so the harness is the three-line `bench` helper `docs/baselines/pre-daemon/workloads.md` fixes, unchanged: one discarded warm-up, then eleven runs, median with min and max beside it.
Using the pre-daemon harness rather than a new one is deliberate: the numbers below sit in the same column as the Phase 0 ones and are comparable with them line by line.

- Fixture: `cargo run --release --example mkfixture -- --out /tmp/mp-fixture-p4 --rows 5000`, the Phase 0 fixture with the Phase 0 flags, 5 501 messages, 25.1 MiB.
- Filesystem: **tmpfs** (`/tmp`), the same as the Phase 0 run, so both columns are lower bounds on a disk-backed directory and neither says anything about a cold page cache.
- Binaries: `~/.cache/mp-oracle/pre-daemon/mp` for the oracle column, `target/release/mp` (`cargo build --release --offline`, `db2d67d`) for the other two.
- **Warm** = a daemon is already up and serving the fixture root; the command is one round trip.
- **Cold** = `mp daemon stop` before every one of the eleven runs, so each includes an on-demand daemon start against a fixture the daemon has never opened.
- No account is configured on the fixture, so both binaries print the SMTP-secret warning on stderr for every run; stdout and stderr are discarded and do not enter the timing.

### The table

Milliseconds, `median min max`.

| workload | oracle (pre-daemon) | warm | cold |
|---|---|---|---|
| `mp --version` (floor) | 7 7 12 | 8 6 8 | 7 7 8 |
| `mp list-messages --mailbox inbox -n 20` (W6) | 42 40 45 | **10** 9 11 | 77 75 80 |
| `mp list-messages --mailbox Bulk -n 5000` (W2) | 51 50 52 | 56 54 66 | 133 132 135 |
| `mp show <ordinary body>` (W4) | 44 42 45 | **11** 10 12 | 78 76 80 |
| `mp show <10 MiB body>` (W4) | 76 73 79 | 73 67 78 | 146 142 148 |
| `mp show --json <10 MiB body>` (W4) | 71 70 73 | 68 64 71 | 138 137 142 |
| `mp dump-mailbox --json -A alpha` (W3) | 86 84 93 | 121 119 148 | 199 194 207 |
| `mp search --local 'body:zolvertrix' -n 100` (W7) | 41 40 43 | **10** 9 11 | 77 76 78 |
| `mp search --local 'body:concrete' -n 100` (W7) | 46 44 47 | **15** 14 16 | 82 81 83 |
| `mp list` (drafts) | 42 40 44 | **9** 7 9 | 39 38 41 |

Every one of these ten was also checked byte for byte against the oracle on the same fixture before it was timed; the five that produce output on a fixture with no account (`list-messages`, `search --local`, `dump-mailbox --json`, `show`, `list`) are identical, stderr and exit code included.

### What the numbers say

**Warm is the gate line and it passes, comfortably, for every small answer.**
A one-shot query that read the store in process cost 42-46 ms and costs 9-15 ms over the socket: about 33 ms saved, which is almost exactly the "config load plus store open" the Phase 0 W6 note budgets.
The daemon holds both, so a warm client pays process start plus a round trip and nothing else.

**A large answer is slower routed, and the crossover is the payload.**
`mp dump-mailbox --json -A alpha` (5 351 NDJSON records, several MiB) goes from 86 ms to 121 ms, and `list-messages -n 5000` from 51 ms to 56 ms; the 10 MiB `mp show` is a wash (76 -> 73 ms) because its cost was always the blob read, which the daemon does once either way.
So the socket costs roughly 35 ms per multi-MiB answer on this host, against the 33 ms it saves per call.
`docs/baselines/decisions/large-payload.md` and `list-transfer.md` are the decisions this measures; nothing here contradicts them, and `mp dump-mailbox` is the one command that pays more than it saves.
It is a batch export, not an interaction, and it is in `BACKLOG.md` as the first candidate if streaming is ever built.

**Cold costs one daemon start, and that start is 67 ms.**
Every cold figure is its warm figure plus 62-68 ms, flat across workloads of very different sizes, which is what a start looks like: fork, bind, load the configuration, open the store.
The exception is `mp list`, whose cold figure (39 ms) is *below* its oracle figure (42 ms), because a drafts listing needs no message store and the daemon it starts opens none either.
There is no Phase 0 cold figure to compare against: W6's cold half is `NOT TAKEN (no root, and the fixture is on tmpfs)` and stays that way, so "within the Phase 0 baselines" is answered for warm and is unanswerable for cold on this host.
Against the *warm pre-daemon* figure, a cold routed command is 1.8x, paid once per daemon lifetime rather than once per command.

### Which `measurements.md` rows stay NOT TAKEN

Unchanged by this unit, all five owner action, all five needing a terminal, a configured account or root:

- **W1**, cursor-move preview p50/p95 - the Phase 5 parity oracle, blocking for that gate.
- **W5**, cold first paint - needed before the daemon adds a start to the launch path.
- **W8**, mutation write plus propagation.
- **W2**, the TUI half (frames over 50 ms during a sync burst); the CLI lower bound above is the partial substitute, and it now has a routed column.
- **W6**, the cold-cache half - needs root (`sysctl -w vm.drop_caches=3`) and a disk-backed data directory. The *cold-daemon* half is taken above and is a different quantity: no daemon running, warm page cache.

No synthetic number was substituted for any of them.

## No command depends on the daemon's working directory

Unit P4-U2.

```sh
cargo test --offline --test daemon_parity_harness --test daemon_autostart
```

16 and 12 pass, 0 fail.
`a_daemon_started_from_root_answers_a_client_standing_elsewhere` and `mp_save_writes_into_the_clients_cwd_with_the_daemon_started_from_root` are the two rows: the daemon is started from `/`, the client stands in a temp dir, and `mp save` writes where the client stands.
`the_save_destination_is_anchored_to_the_clients_working_directory` and `resolve_attachment_paths_yields_absolute_paths_for_relative_entries` cover the absolutisation those rest on.

Passes.

## Existing CLI tests and help snapshots pass

Units P4-U1 to P4-U15.

```sh
touch src/main.rs && timeout 600 cargo build --offline \
  && MP=./target/debug/mp scripts/capture-cli-help.sh | diff - docs/baselines/pre-daemon/cli-help.txt
```

Empty, exit 0: the whole 50-screen help surface is byte-identical to the pre-daemon baseline.
`tests/cli_help_snapshot.rs` passes in the whole-tree run, as do the eight legacy CLI suites the slices rerouted (`cli_read_surface_integration` 8, `cli_selector_contract` 10, `draft_integration` 36, `dump_mailbox_integration` 5, `store_search_integration` 17, `outbox_integration` 31, `imip_integration` 9, `engine_lock_ingest_cli` 1).

Passes.

## Two concurrent CLI commands share one daemon safely

Units P4-U1 and P4-U2.

The gate names "the harness concurrency case".
What the tree has is `a_second_client_reuses_the_daemon_the_first_one_started` (`tests/daemon_autostart.rs`) and `two_concurrent_starts_yield_one_pid_file_and_one_instance_id` (`tests/daemon_lifecycle.rs`): the first proves sequential reuse of one daemon by two clients, the second proves that two clients racing to start one produce exactly one.
Every parity row also runs two binaries against one root in turn.

**Two commands running at the same instant against one daemon has no row of its own.**
It is recorded here rather than claimed: the property is served by the dispatcher's per-connection tasks and by the operation registry, both of which have their own tests, but no test drives two `mp` processes concurrently. It is in `BACKLOG.md`.

## Clippy

```sh
timeout 900 cargo clippy --workspace --offline --all-targets
```

39 warnings, against 39 at `4d8349e` before this unit: no new warning, and none names a file P4-U15 touched.
The seven in `tests/daemon_config.rs` and `tests/daemon_draft_watch.rs` that Phase 3b recorded are still there, still untouchable for the same reason (T-unit files), still in `BACKLOG.md`.

## Housekeeping

`pgrep -af 'mp daemon'` returns nothing after the full run and after the measurement run.
Every suite that starts a daemon points `MAILYPOPPINS_DATA_DIR` at a `TempDir` and stops it in `SandboxRoot::drop`; the measurement run used `/tmp/mp-fixture-p4` and stopped its daemon explicitly.

`rustfmt --edition 2021` was run on the four touched files that were already rustfmt-clean (`src/calendar_cmd.rs`, `src/config_cmd/password.rs`, `src/contacts_cmd.rs`, `tests/architecture_boundaries.rs`) and on none of the three that were not (`src/main.rs`, `src/cutover.rs`, `src/config_cmd/mod.rs`), which is the tree's standing rule.

```sh
timeout 900 cargo install --path . --offline
```

Installs the daemon-backed `mp`.

## What Phase 4 does not answer

- The server leg of `mp search` (LST-06) still runs in the client. It is the one command surface Phase 4 leaves on the direct path, and the largest single item Phase 5 or a slice of its own inherits.
- The two wizards still write `config.toml` themselves. `config.init` and `config.add_account` exist and are called; what has no wire shape is the prompting loop between them.
- `mp config show`'s secret column and token line are still local reads, by `config.get`'s own contract.
- The two interactive measurements escalated out of Phases 0, 1a, 2, 3a and 3b (W1 and W5) are still open, and Phase 5's gate needs both.
- The macOS half of Phase 2's crash and stale-socket recovery line is unchanged and still escalated.
