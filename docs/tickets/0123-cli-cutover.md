---
id: 0123
title: Phase 4 of the daemon migration, the CLI cutover
type: feature
priority: now
status: in-progress
created: 2026-09-10
---

Status: in progress. Eight of Phase 4's fifteen units have landed; this file grows as the rest do.

Sixth ticket of the daemon-first architecture plan (`.agents/workflow/native-gui-daemon/plan.md` section 3.6), after #0118, #0119, #0120, #0121 and #0122.

Phase 3 gave the daemon everything a client would otherwise own.
Phase 4 takes the CLI off the direct path: every command that touches domain state goes through `mp-client` and answers from the daemon, byte-identically to the pre-daemon binary, refusals included.
The daemon stops being a cargo feature and becomes the default build.

## Units done

- **P4-U1 (T+I)** - `f87988d` (build: daemon required by default) and `b8aa9a1` (test: parity harness). The `daemon` feature gate is gone from `Cargo.toml` and the tree; `tests/daemon_parity_harness.rs` plus `tests/support/parity.rs` compare a daemon-backed `mp` against the `pre-daemon` oracle binary byte for byte.
- **P4-U2 (I+T)** - `fc093f0` (on-demand start and client path resolution) and `87754bd` (auto-start, the no-daemon list and harness support). A normal client starts a daemon on demand with one bounded retry sequence, then exits 4 naming `mp daemon run`; `mp daemon *`, `mp dump-keys`, `mp --help`, `mp --version` and `mp config path` are the no-daemon list, and `mp config init` deliberately is not. Every user-supplied path is absolutised client-side, so the daemon's own cwd is irrelevant.
- **P4-U3 (T)** - `510eb94`, the read slice contract: `message.get`, `message.list`, `message.search`.
- **P4-U4 (I)** - `4ad0871`, the read slice on the daemon: `mp show` (+`--json`), `mp list-messages`, `mp dump-mailbox --json`, `mp search --local`, with the NDJSON ordering contract, the path-free field set and the two-run byte-identity preserved.
- **P4-U5 (T)** - `41ca076`, the draft slice contract.
- **P4-U6 (I)** - `3f27c54`, the draft slice on the daemon: `mp new`, `list`, `validate`, `mark-approved`, `mark-draft`, `path`, `edit`, `reply [--all]`, `forward` and the bare-selector dry run. The `draft.*` family grows from one method to nine, with the wire shapes in `crates/mp-protocol/src/draft.rs`.

- **P4-U7 (T)** - `380427a`, the message-mutation slice contract: `MESSAGE_MUTATION_METHOD_SPECS`, `draft.discard` as the tenth `draft.*` method, and the two additive changes to `message.materialise_attachment`.
- **P4-U8 (I)** - the message-mutation slice on the daemon: `mp archive`, `mp delete [--force|--sent]`, `mp open` and `mp save [-o]`. `message.archive` and `message.delete` are durable commands that resolve the backend before they touch the store and drain the op they owe the server before answering, which is what keeps the synchronous `run_and_settle` UX. `draft.discard` carries both the single delete and the `--sent` sweep. The materialiser gained `{selector, mailbox?}` addressing and a `name` field, and `mp open` and `mp save` are built on it: the daemon prepares one file per part, the client launches the opener or writes the copy, and the `_1` rule for two parts sharing a name is the client's, applied within one call.

- **P4-U9 (T)** - `2ae504c`, the sync/watch slice contract: `SYNC_METHOD_SPECS`, `mailbox.list_server` as the second `mailbox.*` method, `message.list_server` in an array of its own, `Phase::as_str` and the four client wordings.
- **P4-U10 (I)** - the sync/watch slice on the daemon: `mp sync [-n|--mailbox|--dry-run|--all-accounts]`, `mp fetch`, `mp list-mailboxes` and `mp watch [--mailbox|--timeout]`. `sync.quick` and `sync.full` are durable operations running the #0114 tick, one `operation.progress` per phase; `sync.watch` is a client-scoped one over bounded IDLE rounds. `--all-accounts` stays a loop in the client, `--timeout` stays client-side, and `mp sync` still passes no body-fetch deadline. A local-only account is `-32006` with `state: "local_only"` and renders as the skip line at exit 0; a blocked runtime is a *successful* operation answering `{blocked: true, outcome: null}`. Every line `mp sync` prints now comes from `mp_client::format`, which gained `TAIL_LABEL`, `outbox_drain_line`, `mutations_drain_line`, `drain_failed_line` and the dry-run rendering.

`265f6f7` (test: stop auto-started daemons in legacy CLI suites) sits between P4-U4 and P4-U5 and belongs to no unit; see below.

Still open: P4-U11..U15, the send and admin slices, then the direct-path deletion and the phase gate.

## Approved test edits

The convention is that an implementer never edits a T unit's file except to fix a contract error approved in writing.
These are the edits approved so far in Phase 4.

- **`tests/daemon_draft_slice.rs` `draft_list_filters_by_status`** sorts the returned ids with `sort_unstable` and compared them against the unsorted literal `vec![NO_SUBJECT, VALID]`. The expectation is now `vec![VALID, NO_SUBJECT]`, which is the byte order of `a0000000000000d1` before `d000000000000051`. The assertion the test makes is unchanged; only the literal was written in the wrong order.
- **`tests/daemon_draft_watch.rs` `the_one_draft_method_declares_its_name_kind_since_and_cancel_scope`** asserted `DRAFT_METHOD_SPECS` is exactly `["draft.approve"]`, which was true at P3b-U9 and stopped being true at P4-U5. `DRAFT_METHODS` now lists the nine names in method-name order, the test is `the_draft_methods_declare_their_names_kind_since_and_cancel_scope`, and the file header no longer says the family has one method. `draft.approve` is still the only one this file pins in full; `tests/daemon_draft_slice.rs` owns the other eight.
- **`tests/cli_read_surface_integration.rs`, `tests/dump_mailbox_integration.rs`** (`265f6f7`) and **`tests/cli_selector_contract.rs`** (`3f27c54`) wrap their temporary tree in `support::parity::SandboxRoot`. Since P4-U2 these suites start a daemon on demand and used to drop the tree out from under it, so a full `cargo test --workspace` finished with eleven `mp daemon run` processes each holding a deleted directory open. `SandboxRoot` owns the `TempDir` and stops the daemon in `Drop`, best-effort and without asserting, because a panic while another panic unwinds would abort the process and hide the assertion the test was about. Every assertion is kept; only the fixture type changes.
- **`tests/daemon_read_only_methods.rs`** (`fc093f0`) switches to the auto-start path for the same reason: the suite predates P4-U2 and its expectations about who starts the daemon no longer held.
- **`tests/daemon_handles.rs`** (after P4-U8) reconciles `MATERIALISED_KEYS` with the additive `name` field the P4-U7 contract requires: the constant is now the sorted `["bytes", "expires_at", "handle", "name", "path"]`, `materialised()` additionally asserts `result["name"] == expected_name` (the helper already received it), and the file header lists the materialiser shape as `{handle, path, name, bytes, expires_at}`. Every assertion the file made is kept; one is added.
- **`tests/daemon_draft_slice.rs`** (after P4-U8) finishes the reconciliation `380427a` began: `DRAFT_COMMANDS` gains `draft.discard` (six, in method-name order), since the P4-U7 contract declares it a `Command`, and `every_path_a_draft_method_returns_is_a_draft_path` gains a tenth call so the coverage list matches `DRAFT_METHODS` again. A discard answers `{account, id, selector, status}` and carries no path, so it is exempted from the at-least-one-path check the way `draft.validate` is, with the reason in the assertion message; the call runs last in the list because it removes the draft the earlier calls address, and the slice owns its fixture root.
- **`tests/daemon_parity_harness.rs` `UNMIGRATED`** (after P4-U10) names `outbox list` where it named `list-mailboxes`. The third row exists to compare a command that *needs* a daemon and has not been migrated onto one, so it had to move the moment the sync/watch slice routed `mp list-mailboxes`; `mp outbox list` is byte-identical over the harness's empty root and belongs to the send slice (P4-U12), which has yet to take it. The assertion is unchanged.
- **`tests/engine_lock_ingest_cli.rs`** (after P4-U10) wraps its temporary tree in `support::parity::SandboxRoot`, for the reason the three suites above did: the `mp sync` it spawns is a daemon client now, so the child starts a daemon that outlived the tree. It matters more here than elsewhere, because that daemon is forked while the test holds the account's engine lock and therefore inherits a copy of the locked descriptor. Every assertion is kept, the paths are unchanged, and only the guard's `Drop` is added.
- **`tests/daemon_autostart.rs` `mp_save_writes_into_the_clients_cwd_with_the_daemon_started_from_root`** (P4-U8) is no longer `#[ignore]`d. It could not pass as written: it ran against a bare temporary root with no configuration and no store and asked for `mp://alpha/inbox/msg@example.com`, which nothing seeds. It now seeds `support::mutation_fixture::seed` into that root and asks for `mp://alpha/inbox/bericht@example.com`; the daemon-started-from-`/` half is unchanged, and so is the assertion.

## P4-U6 follow-ups

Recorded here rather than fixed in the implementing commit, and none of them changes an observable byte:

- `collisions` is reported only on `draft.list`. The other `draft.*` methods scan the same directory and could see the same colliding ids, and say nothing.
- `draft.list` and `draft.validate` each parse the draft file twice, once for the projection and once for the diagnostics.
- `resolve_body_signature` is still a delegate rather than a method of its own, so the signature resolution a send needs is reached through the draft path.
- `draft.create` takes the from-address from `account.default_from`, which is the right source but is read directly instead of through the configuration accessor the rest of the family uses.

## P4-U10 decisions and follow-ups

The decision the plan left to this unit: **`mp watch --mailbox` is narrowed to INBOX**, the route the plan recommends, rather than building an on-demand client-scoped IDLE connection. It is recorded in `BACKLOG.md`, which the warning text names, and both sides carry it: the client warns on stderr and rewrites the mailbox before it calls, the daemon refuses anything else with `-32602`.

The rest, none of them changing a byte a parity row asserts:

- A **fake IMAP backend fixture** is what the successful half of this slice needs, exactly as P4-U8 recorded for the mutation slice. Recorded in `BACKLOG.md` now that two slices want it.
- **`mp list-mailboxes` over an empty configuration changed its wording**, which is why `tests/daemon_parity_harness.rs`'s unmigrated control row moved: the pre-daemon binary tried to load credentials for an account named `""` and printed the secret-store sentence, and the daemon refuses `-32005 no account named  is configured` like every other unknown account. No configured account can reach it.
- **The retention sweep after a sync is still the client's**, and is the one store `mp sync` still opens. Moving it into the pass would put the sweep's report lines on the wire; P4-U15 owns it.
- **A pass with a `--mailbox` subset or `--dry-run` takes the guarded path even when this daemon holds the account's engine lock**, because the runtime's tick carries neither, so such a run is answered `blocked` by this daemon's own runtime. Account runtimes are behind `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES` until Phase 5, so no default run reaches it.
- **`mp watch --timeout N` counts from the call rather than from the IDLE**, since the wait is the client's; the pre-daemon binary handed N to the IDLE itself. The exit code, the message and the flag are unchanged.
- The daemon's watch runs **60-second IDLE rounds** rather than one unbounded IDLE, because the session is driven on a thread of its own and a cancellation can only be observed between rounds.
- **The TUI adopted nothing**, because it prints none of these lines: `rg 'after sync|drain failed|↻' src/tui/` finds only a comment. Its own drain suffix is `sync_status_line`'s, which P3b-U6 already put in `mp_client::format`.

## P4-U8 follow-ups

From the P4-U7 report and from the implementation:

- A **fake IMAP fixture** would unlock the success paths of MSG-01 and MSG-02: the local commit, the synchronous drain and the rollback when the server refuses are all out of reach of a fixture with no server, and stay covered only by the `pending_ops` unit tests.
- The **`is mid-send: outbox row` refusal** of `draft.discard` is untested until the send slice (P4-U11) can put a draft in an outbox row.
- `mp open` hands the daemon's handle path straight to the opener, so **two parts sharing a name are two files in two handle directories** rather than `report.pdf` and `report_1.pdf` in one. Nothing observable depends on the name there and no row asserts it, but it is the one place `mp open` and `mp save` now differ.
- A **chained error** raised inside the daemon reaches the user as its top line only: `refusal()` rebuilds the error from `RpcError::message`, while the pre-daemon binary printed anyhow's `Debug`, which appends a `Caused by:` block. Every refusal this slice asserts is a single unchained error, so no row moves; a server refusal during a drain could.
- The routed mutation budget is 300 s against the ordinary 10 s, because `message.archive` spans a round trip to the mail server. It is a constant rather than a configured value.

## Validation

`timeout 900 cargo test --workspace --offline` after the two test edits: 1892 passed, 0 failed.
`pgrep -af '[m]p daemon'` empty afterwards, which is the property `SandboxRoot` exists to hold.

After P4-U8: 1929 passed in the green targets (1892 + the 35 of `tests/daemon_mutation_slice.rs` + the un-ignored auto-start test + one new `capabilities` name in the lib's own suite), with `tests/daemon_handles.rs` and `tests/daemon_draft_slice.rs` failing until the two reconciling edits above.

After those edits: `timeout 900 cargo test --workspace --offline` -> 1928 passed, 0 failed, `pgrep -af '[m]p daemon'` empty.

After P4-U10: `timeout 900 cargo test --workspace --offline` -> 1968 passed, 0 failed (1928 + the 38 of `tests/daemon_sync_slice.rs` + two unit tests in `src/daemon/methods/sync.rs`), `--test daemon_sync_slice` green three times running, the help walk byte-identical to `docs/baselines/pre-daemon/cli-help.txt`, clippy clean on every touched file, and `pgrep -af '[m]p daemon'` empty.
