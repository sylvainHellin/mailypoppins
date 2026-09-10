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

`265f6f7` (test: stop auto-started daemons in legacy CLI suites) sits between P4-U4 and P4-U5 and belongs to no unit; see below.

Still open: P4-U9..U15, the sync/watch, send and admin slices, then the direct-path deletion and the phase gate.

## Approved test edits

The convention is that an implementer never edits a T unit's file except to fix a contract error approved in writing.
These are the edits approved so far in Phase 4.

- **`tests/daemon_draft_slice.rs` `draft_list_filters_by_status`** sorts the returned ids with `sort_unstable` and compared them against the unsorted literal `vec![NO_SUBJECT, VALID]`. The expectation is now `vec![VALID, NO_SUBJECT]`, which is the byte order of `a0000000000000d1` before `d000000000000051`. The assertion the test makes is unchanged; only the literal was written in the wrong order.
- **`tests/daemon_draft_watch.rs` `the_one_draft_method_declares_its_name_kind_since_and_cancel_scope`** asserted `DRAFT_METHOD_SPECS` is exactly `["draft.approve"]`, which was true at P3b-U9 and stopped being true at P4-U5. `DRAFT_METHODS` now lists the nine names in method-name order, the test is `the_draft_methods_declare_their_names_kind_since_and_cancel_scope`, and the file header no longer says the family has one method. `draft.approve` is still the only one this file pins in full; `tests/daemon_draft_slice.rs` owns the other eight.
- **`tests/cli_read_surface_integration.rs`, `tests/dump_mailbox_integration.rs`** (`265f6f7`) and **`tests/cli_selector_contract.rs`** (`3f27c54`) wrap their temporary tree in `support::parity::SandboxRoot`. Since P4-U2 these suites start a daemon on demand and used to drop the tree out from under it, so a full `cargo test --workspace` finished with eleven `mp daemon run` processes each holding a deleted directory open. `SandboxRoot` owns the `TempDir` and stops the daemon in `Drop`, best-effort and without asserting, because a panic while another panic unwinds would abort the process and hide the assertion the test was about. Every assertion is kept; only the fixture type changes.
- **`tests/daemon_read_only_methods.rs`** (`fc093f0`) switches to the auto-start path for the same reason: the suite predates P4-U2 and its expectations about who starts the daemon no longer held.
- **`tests/daemon_autostart.rs` `mp_save_writes_into_the_clients_cwd_with_the_daemon_started_from_root`** (P4-U8) is no longer `#[ignore]`d. It could not pass as written: it ran against a bare temporary root with no configuration and no store and asked for `mp://alpha/inbox/msg@example.com`, which nothing seeds. It now seeds `support::mutation_fixture::seed` into that root and asks for `mp://alpha/inbox/bericht@example.com`; the daemon-started-from-`/` half is unchanged, and so is the assertion.

## P4-U8 blockers, unresolved

Two committed test files contradict the P4-U7 contract, and neither is on the approved-edit list above, so they were left failing rather than weakened:

- **`tests/daemon_handles.rs`** (5 tests, all through `materialised()` at line 1503) asserts a materialisation answers exactly `MATERIALISED_KEYS = ["bytes", "expires_at", "handle", "path"]`. P4-U7 requires the additive `name` field, asserted three times in `tests/daemon_mutation_slice.rs`, and no implementation satisfies both. The reconciliation is one line, `MATERIALISED_KEYS` gaining `"name"`, and `materialised()` already takes the `expected_name` it could then assert against.
- **`tests/daemon_draft_slice.rs`** (2 tests) was reconciled to the ten-method family by `380427a` only in part: `DRAFT_METHODS` lists ten, while `DRAFT_COMMANDS` still lists five and `every_path_a_draft_method_returns_is_a_draft_path` still calls nine. `draft.discard` is a command by the P4-U7 contract (`tests/daemon_mutation_slice.rs::draft_discard_is_the_tenth_method_of_the_draft_family`), so `DRAFT_COMMANDS` must gain it, and the coverage list must gain a `draft.discard` call.

Both are bookkeeping in test files that the contract grew underneath, not design questions.

## P4-U6 follow-ups

Recorded here rather than fixed in the implementing commit, and none of them changes an observable byte:

- `collisions` is reported only on `draft.list`. The other `draft.*` methods scan the same directory and could see the same colliding ids, and say nothing.
- `draft.list` and `draft.validate` each parse the draft file twice, once for the projection and once for the diagnostics.
- `resolve_body_signature` is still a delegate rather than a method of its own, so the signature resolution a send needs is reached through the draft path.
- `draft.create` takes the from-address from `account.default_from`, which is the right source but is read directly instead of through the configuration accessor the rest of the family uses.

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

After P4-U8: 1929 passed in the green targets (1892 + the 35 of `tests/daemon_mutation_slice.rs` + the un-ignored auto-start test + one new `capabilities` name in the lib's own suite), with `tests/daemon_handles.rs` and `tests/daemon_draft_slice.rs` failing for the reason recorded above.
