---
id: 0123
title: Phase 4 of the daemon migration, the CLI cutover
type: feature
priority: now
status: done
created: 2026-09-10
---

Status: done. All fifteen units have landed; the gate evidence is [docs/baselines/phase4-gate-evidence.md](../baselines/phase4-gate-evidence.md).

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


- **P4-U11 (T)** - `c9c91dc`, the send slice contract: `SEND_METHOD_SPECS`, the `mp_protocol::send` result types, the six `mp_client::format` wordings and the `MAILYPOPPINS_DAEMON_FAKE_TRANSPORT` hook.
- **P4-U12 (I)** - the send slice on the daemon: `mp send [-y]`, `mp send --invite`, `mp send-approved [-y] [--all-accounts]` and `mp outbox list|retry|discard`. Six methods, all durable: three sends and a retry as operations, the listing as a query, the discard as a command. The preview, the `[y/N]` prompt, the invitation's `UID` and `--all-accounts` all stay in the client, which renders the preview from `draft.preview` and the batch listing from `draft.list`; the transport, the outbox and the drafts directory are the daemon's. `mailypoppins::invite::plan_invite` is the one validator both sides use, so `ANO-4` is refused in the same sentence and the same order on either. The CLI send paths take no hold (`ANO-7`), and a caller that sends one is `-32602`.

- **P4-U13 (T)** - `e633d45`, the admin slice contract: `CONTACT_METHOD_SPECS`, `CALENDAR_METHOD_SPECS`, `DIAGNOSTIC_METHOD_SPECS`, and the config family from six methods to nine.
- **P4-U14 (I)** - `4d8349e`, the admin slice on the daemon; see the section below.
- **P4-U15 (I)** - the direct-path deletion and the gate; see the section below.

`265f6f7` (test: stop auto-started daemons in legacy CLI suites) sits between P4-U4 and P4-U5 and belongs to no unit; see below.

### The unit table

| unit | kind | commit | subject |
|---|---|---|---|
| P4-U1 | T+I | `f87988d`, `b8aa9a1` | the daemon required by default, the parity harness |
| P4-U2 | I+T | `fc093f0`, `87754bd` | on-demand start, the no-daemon list, path absolutisation |
| P4-U3 | T | `510eb94` | the read slice contract |
| P4-U4 | I | `4ad0871` | the read slice on the daemon |
| P4-U5 | T | `41ca076` | the draft slice contract |
| P4-U6 | I | `3f27c54` | the draft slice on the daemon |
| P4-U7 | T | `380427a` | the message-mutation slice contract |
| P4-U8 | I | (see above) | the message-mutation slice on the daemon |
| P4-U9 | T | `2ae504c` | the sync/watch slice contract |
| P4-U10 | I | `dd8d63f` | the sync/watch slice on the daemon |
| P4-U11 | T | `c9c91dc` | the send slice contract |
| P4-U12 | I | `9d576c8` | the send slice on the daemon |
| P4-U13 | T | `e633d45` | the admin slice contract |
| P4-U14 | I | `4d8349e` | the admin slice on the daemon |
| P4-U15 | I | `5859942`, `db2d67d` | direct-path deletion, the boundary test, the gate |

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
- **`tests/daemon_parity_harness.rs` `UNMIGRATED`** (after P4-U12) names `contacts stats` where it named `outbox list`, for the reason it named `outbox list` in the first place: the third row has to be a command that needs a daemon and has not been migrated, and the send slice took `mp outbox list`. `mp contacts stats` is byte-identical over the harness's empty root and belongs to the admin slice (P4-U14). The assertion is unchanged; the comment above the constant records both moves.
- **`tests/daemon_send_slice.rs` `a_routed_send_delivers_retires_the_draft_and_files_the_copy`** (after P4-U12) is reconciled with `draft::settle_sent_draft`, which retires a fully delivered draft by *deleting* the file rather than by leaving `status: sent` behind it. The row now asserts the pair the deletion rests on: the file is gone, and the outbox holds exactly one row the send minted, in `done`. Its ledger assertion moves with it, from `appends() == 1` to `appends_of(minted Message-ID) == 1`: a send drains the account's outbox on its way out, so the seeded row waiting on its APPEND (`APPENDING_ROW`) files its copy in the same run and a total of two is the drain doing its job. SND-09 is about this message's copy, and counting by Message-ID says so. The row is routed-side only, so no parity expectation moves.
- **`tests/support/send_fixture.rs`** (after P4-U12) gives the approved draft a second recipient, so `a_routed_send_whose_recipient_is_refused_is_partly_delivered` has a partial outcome to reach: a draft with one recipient is delivered whole or refused whole, and the fake transport refuses exactly `REJECTED` (`carol@example.com`), which no draft carried. `seed` rewrites `freigabe.md` in place with `cc: carol@example.com`, restoring the modification time afterwards because the drafts index orders by `mtime DESC, id ASC`. The `to:` line stays `ivana@example.com`, so `mp send-approved`'s `freigabe.md -> ivana@example.com` does not move, and the rewrite lives in the send slice's own fixture rather than in `draft_fixture`, so no other slice sees it. The fixture also gains `APPROVED_FILE`, `SEEDED_ROWS` and `all_rows`, the last because `unfinished_rows` hides a row that reached `done`.
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

After the two reconciling test edits: `timeout 1200 cargo test --workspace --offline` -> 2020 passed, 0 failed, 4 ignored, `--test daemon_send_slice` -> 50 passed three times running, and `pgrep -af '[m]p daemon'` empty afterwards.

After P4-U12: `timeout 900 cargo test --workspace --offline --no-fail-fast` -> 2018 passed, 2 failed, 4 ignored (1968 + the 48 passing rows of `tests/daemon_send_slice.rs` + two unit tests in `src/daemon/methods/send.rs`). The two failures are the rows above, which contradict `draft::settle_sent_draft` and the fixture's own recipient list; `--test daemon_send_slice` gives the same 48/2 three times running, `outbox_integration` and `imip_integration` are untouched and green, the help walk is byte-identical to `docs/baselines/pre-daemon/cli-help.txt`, clippy is clean on every touched file, and `pgrep -af '[m]p daemon'` is empty afterwards.

## P4-U12 decisions and follow-ups

Decisions this unit had to take, none of them settled by the plan:

- **`send.invite` takes three parameters the plan's shape does not list**: `uid`, `signature` and `no_signature`. The preview is the client's, `invite::generate_uid` mints a UID per run, and a user who reads `UID: x` must be sent `UID: x`, so the client mints it while previewing and sends it; the daemon mints one only when a caller previewed nothing. The two signature flags are global CLI flags with no draft body to live in, and they travel exactly as the `draft.*` writers' do.
- **A retry accepts one state more than `outbox::retry` does.** `outbox::retry` re-arms a `failed` row and refuses everything else, which is right and not enough: a `sent_pending_append` row whose APPEND has already been attempted (`attempts > 0`) is the row whose copy may or may not have been filed, and re-driving that APPEND behind the Message-ID dedup search is exactly what `mp outbox retry` is for. A row nobody has attempted is still refused, because somebody may be inside its APPEND right now.
- **A retry admitted while another retry for the same account is running is not refused on the row's state**, because that state is the other retry's to change and the two calls race by construction. One retry per account runs at a time behind a `tokio::sync::Mutex`, an admission ticket says one is in flight, and the second operation settles on whatever the first left.
- **`mp outbox retry` drains against `unix_now() + BACKOFF_MAX_SECS`.** A failed APPEND arms a 30-second backoff, so a retry that passed the real clock would re-arm the row and then skip it. `send::resume_outbox_at` and `drain_account_at` take the clock; every periodic driver still passes the real one.
- **All three outbox subcommands call `send.outbox_list` first.** The pre-daemon command looked for a store file before it read the action, so an account that never queued anything gets `nothing has been queued for <account> yet` and exit 0 from `retry` and `discard` too. That costs one round trip on a mutating subcommand and keeps the sentence.

The rest:

- **`mp outbox list` lost the colour inside its padded state column**, and nothing else: `mp_client::format` is colourless by contract and `colored` drops a width specifier as soon as colours are on, so the pre-daemon binary already printed two different layouts. Recorded in `docs/lessons-learned.md`.
- **`mp send-approved` still loads an SMTP config per account in the client**, purely to reproduce the `⚠ Could not load SMTP config: …` line the pre-daemon loop printed before each account's batch. It is the last transport load on the CLI's path and it goes with the rest in P4-U15.
- **The `-y`-less runs reach the daemon before they can be declined**, which is what `MAILYPOPPINS_DAEMON_REQUIRE=1` measures: `mp send` calls `draft.preview` to resolve and preview, and `mp send --invite` opens its session after the `ANO-4` refusal and before the preview, so a Graph invitation still costs no session.
- **A selector with no account is parsed in the client.** `mp send -A nope <id>` binds an account named `""`, and `selector::parse_in` has always answered that with `no account for <id>: name one with -A/--account or configure a default`; sending the empty name over the socket would answer it with `account_unknown` instead.
- **Two assertions of `tests/daemon_send_slice.rs` cannot pass against this tree** and are left failing rather than weakened; see below.

### The two failing rows

`tests/daemon_send_slice.rs:2125`, in `a_routed_send_delivers_retires_the_draft_and_files_the_copy`, asserts `draft_status(root, alpha, "freigabe.md") == "sent"` after a successful routed send.
`draft::settle_sent_draft` (`src/draft.rs`) *deletes* the file when the send reached every recipient and got an outbox row: `mark_draft_sent` writes `status: sent` and then `fs::remove_file` retires it.
The fixture's approved draft has one recipient, the fake transport accepts it, so the file is gone and `draft_status` panics reading it.
That behaviour is the library's and predates the daemon, the pre-daemon binary does the same, and no parity row covers a successful send, so the assertion describes a tree that does not exist rather than a routing difference.
Everything else in that test passes: the success line, the single `append` in the ledger, and the absence of a path in the output.

`tests/daemon_send_slice.rs:2188`, in `a_routed_send_whose_recipient_is_refused_is_partly_delivered`, asserts the send prints `⚠ Partial send:` after arming the fake transport to refuse `carol@example.com` (`send_fixture::REJECTED`).
The draft it sends is the same approved draft, whose only recipient is `ivana@example.com`; `carol@example.com` is a recipient of the *seeded outbox rows*, not of any draft.
The fake refuses exactly the addresses it is given and accepts the rest, which is the contract `tests/support/send_fixture.rs` states, so the send succeeds in full and no partial outcome can occur.
The row's second half, which asserts `mp outbox list` names the recipient who never got it, passes from the seeded partly-delivered row.

Both would be fixed by a change to the test file - rejecting `ivana@example.com` in the second, dropping or inverting the file assertion in the first - which an implementer may not make.

Both were fixed afterwards, under the two "Approved test edits" entries above, and neither by rejecting `ivana@example.com`: the fixture's approved draft got a second recipient instead, so the partial outcome is a property of the draft rather than of a refusal aimed at the only address it had. The first row also hid a third contradiction behind the panic - `appends() == 1` counts the whole ledger, and a send drains the outbox - which the same edit reconciles.


## P4-U14: the admin slice on the daemon

Eight methods land: `contact.{rebuild,search,stats}`, `calendar.{rebuild,rsvp}`, `diagnostic.store_gc`, and `config.{cutover,oauth2_login,reset_secrets}`, which take the config family from six to nine.
Every one is `since: 1` and `CancelScope::Durable`, every one takes a required `account` and none takes an `all_accounts`: the "default account" and every all-accounts loop are the client's, over the configured accounts in configuration order.
`mp config {init,add-account,show,set-password,oauth2-login,reset-secrets}`, `mp contacts {search,rebuild,stats}`, `mp calendar rebuild`, `mp invite accept|tentative|decline`, `mp store gc` and `mp cutover` all route by default; `mp config path` stays on the no-daemon list and is now the parity harness's only unmigrated control row.

Implementation is 1 386 inserted / 173 deleted lines across 18 files, of which 13 are the deletions and re-indentations of the renderer extraction below.

### Decisions this unit had to take

- **The renderers moved before the callers did.** `mp contacts search` prints three Nerd Font glyphs, `mp cutover` prints five, and retyping any of them into a second copy is how a parity row dies. So `contacts_cmd`, `calendar_cmd` and `cutover` grew `print_*` functions that take plain data, the direct handlers call them with data read from the store, and the routed client calls them with data read off the wire. One copy of every literal, whichever process did the reading.
- **`contact.rebuild` does not refuse a storeless account**, though `contact.search` and `contact.stats` do. `build_index_for_account` treats an account with no store as an empty index and `mp contacts rebuild` with no `--account` has always walked every configured account including those; refusing would have ended the walk at the first one.
- **`mp contacts search` and `mp contacts stats` against a storeless account now fail** where they printed an empty index. `tests/daemon_admin_slice.rs` pins `-32006` for both, no parity row covers the case, and the alternative was a method that answered about an account `account.list` calls not ready. The behaviour change is here rather than hidden.
- **`config.reset_secrets` sorts the token-cache walk.** The pre-daemon binary printed `read_dir` order, which is only deterministic because a real installation has one cache per account; sorting is one line and makes the answer reproducible. It cannot move a parity byte on any fixture with fewer than two caches, and the admin fixture has one.
- **`mp config reset-secrets` calls `config.get` before it prints its banner**, which is what lets the declined branch satisfy `MAILYPOPPINS_DAEMON_REQUIRE`, and it is the account list the walk needs anyway. `mp config init` and `mp config add-account` call it for the same reason and for the two facts their first line prints.
- **`mp config show` renders `config.get`'s effective configuration**, deserialised back into a `GlobalConfig`. The secret probes, the token-cache status and the signature listing are still read in the client; they are the last local reads on this command's path and they go with the rest in P4-U15.
- **`mp config oauth2-login` no longer runs its post-login connection tests.** They need the access token, which the daemon now holds and the settled result deliberately does not carry, and `tests/daemon_admin_slice.rs` records them as the account slice's subject. Nothing pinned them; the three lines the command prints are pinned, in `mp_client::format`.
- **The browser is still not launched.** `INT-04` puts the launch in the client, and the client is where it would go, but the pre-daemon binary only ever printed the block: adding a launch would be a behaviour change with no parity row to catch it. The rendering is a pure function of the progress payload, so a GUI can do both.

### Approved test edit

`tests/daemon_config.rs` (P3b-U8) enumerated the config family as six names in a `[&str; 6]`, which `tests/daemon_admin_slice.rs` contradicts by construction (`const _: () = assert!(CONFIG_METHOD_SPECS.len() == 9)`).
The constant grew to nine and two assertion messages dropped the word "six"; no assertion of that suite changed. `git diff --stat e633d45..HEAD -- tests/` is that one file, 13 insertions and 5 deletions.

### Follow-ups

- The client still opens the secrets backend for `mp config show`'s `(not set)` / `****` column, and still writes `config.toml` itself in both wizards. Both are P4-U15's.
- `mp config oauth2-login`'s IMAP, SMTP and Graph checks want a home in the account slice.

### Validation

`timeout 1800 cargo test --workspace --offline` -> 2 068 passed, 0 failed, 1 ignored (2 020 + the 44 of `tests/daemon_admin_slice.rs` + four new unit tests).
`--test daemon_admin_slice` -> 44 passed three times running.
The help walk (`touch src/main.rs && cargo build --offline && MP=./target/debug/mp scripts/capture-cli-help.sh | diff - docs/baselines/pre-daemon/cli-help.txt`) is empty.
`timeout 600 cargo clippy --workspace --offline --all-targets` reports nothing on any touched file.
`pgrep -af '[m]p daemon'` is empty after the full run.

## P4-U15: the direct-path deletion and the gate

Three commits: `5859942` (the deletions), `db2d67d` (the boundary test), and this one (the latency measurement and the documentation).

### What was deleted

1 029 lines out, 64 in, across eight files, and `cargo build --offline` reports **zero warnings**, dead-code warnings included.

- `src/main.rs`, thirteen items that carried `#[allow(dead_code)]` naming this unit: `format_unix_time`, `ARCHIVE_MAILBOX`, `drafts_store`, `drafts_store_reporting`, `print_skipped_drafts`, `reindex_drafts`, `resolve_draft_arg`, `list_message_groups`, `resolve_received_arg`, `configured_mailbox_names`, `drain_queues_cli`, `sync_one_account`, `run_store_gc`, plus the three `use` lines they were the last users of.
- `src/config_cmd/oauth2.rs` and `src/config_cmd/reset.rs`, whole: `config.oauth2_login` and `config.reset_secrets` serve those commands since P4-U14, and nothing else called `cmd_oauth2_login` or `cmd_reset_secrets`.
- `src/config_cmd/password.rs`'s `cmd_set_password`. The file stays for `check_kind`, `prompt` and `stored_line`, which the routed client calls.
- `src/contacts_cmd.rs`'s `handle_rebuild` and `handle_stats`, `src/calendar_cmd.rs`'s `handle_rebuild`, `src/cutover.rs`'s `handle_cutover`. Every `print_*` those handlers shared with the routed client stays, which is why no literal moved and no parity row does either.

### What moved behind the daemon

The post-sync retention sweep, the one store `mp sync` still opened (the P4-U10 follow-up that named this unit).
It is `diagnostic.store_gc` now, on the connection the sync already follows, with the options it always used (`SweepOptions::default()`, neither a dry run nor forced) and `manual = false` on the report, so it stays as quiet as it was.
`routed_store_gc`'s wire decode became `sweep_outcome`, shared by both callers, so the two paths cannot drift.
An account the daemon calls storeless is the silent case the local `path.exists()` check used to be; a refusal is a `warn!` line, as a sweep failure always was.

### The boundary test

`tests/architecture_boundaries.rs` gains a second half: a walk of `src/main.rs`, `src/cutover.rs` and `src/config_cmd/` for the 23 symbols that open a store, a secret backend, a network backend or an engine lock, compared against `CLI_ENGINE_RESIDUE`, an inline `(file, symbol, reason)` table.
Comments and `#[cfg(test)] mod` blocks are stripped first, so a unit test that opens a store is not a CLI handler.
The TUI is deliberately outside this list until Phase 5 (#0124), and the test says so; its residue is the first half of the same file.

3 tests to 6, and `git diff --stat 4d8349e..HEAD -- tests/` is that one file.

### The residue, and the deviation from "allow-list to zero"

The gate asks for zero. It is at seventeen, in four groups, and **none of the four can be closed without editing a T unit's test file**, which an implementer may not do. That is the deviation, recorded here rather than left to a diff.

- **(a) The server leg of `mp search`**, six rows. `docs/parity-matrix.md` LST-06 is `not started`: the read slice (P4-U3/U4) contracted `mp search --local` and nothing else, so no unit of Phase 4 ever owned the server search. `MESSAGE_READ_METHOD_SPECS` is pinned at three and `MESSAGE_SERVER_METHOD_SPECS` at one, so the `message.search_server` it wants cannot be added here. It is the one command surface Phase 4 leaves on the direct path.
- **(b) The startup preamble**, three rows. `init_secrets_backend` and the `SmtpConfig::load` under it print the `⚠ Could not load SMTP config: …` pair and the undecryptable-store exit that every `mp` invocation has printed since long before the daemon. They run before any socket, and they run on the no-daemon list too (`mp config path`, `mp daemon *`), so routing them would either need a daemon for a command that must never need one or move bytes on it. `mp send-approved`'s per-account load is the same line one round trip too early to come off `send.approved`, and `config.get`'s key set is pinned at four so it cannot carry an `smtp_warnings` field either. `secrets_path()` is in the list because the scan looks for the module; it opens nothing.
- **(c) `mp config show`'s three probes**, three rows. `tests/daemon_config.rs` contracts `config.get` *not* to look a secret up, in as many words and with the reason ("answering it for every account on every read is exactly the pattern that turns a redacted read into a secret read"), and `tests/daemon_admin_slice.rs` pins the config family at nine methods with a compile-time assertion.
- **(d) The two wizards**, five rows. `config.init` and `config.add_account` exist and are what the wizards ask for their path and their account list; what has no wire shape is the prompting loop, whose intermediate results steer the next prompt.

### Decisions this unit had to take

- **The post-sync sweep goes over the wire rather than staying local.** It was the only residue with a wire shape already built for it, so it was the only one where "move it behind the daemon" was an implementation rather than a contract change. It costs `mp sync` one extra operation per account.
- **No new method was added.** Every family the residue would need is closed by a compile-time assertion in a T unit's file (`CONFIG_METHOD_SPECS.len() == 9`, `DIAGNOSTIC_METHOD_SPECS.len() == 1`, and the `message.*` arrays). Adding one would have meant editing a pinned test to make an implementation fit, which is the thing the convention exists to prevent, so the residue is recorded instead.
- **`src/config_cmd/password.rs` was kept and pruned rather than deleted.** Three of its four functions are the routed client's.
- **The residue table is inline in the test, not a fixture file.** The reason column is the point of it, and a reason in a fixture file is a reason nobody reads at the failure site.

### Latency

Full table, method and provenance in [docs/baselines/phase4-gate-evidence.md](../baselines/phase4-gate-evidence.md); `hyperfine` is not installed on this host, so the harness is the Phase 0 `bench` helper unchanged, which is what makes the columns comparable.

Median of eleven, milliseconds, on the Phase 0 fixture (5 501 messages) on tmpfs:

| workload | oracle | warm | cold |
|---|---:|---:|---:|
| `mp --version` (floor) | 7 | 8 | 7 |
| `mp list-messages --mailbox inbox -n 20` | 42 | **10** | 77 |
| `mp show <ordinary body>` | 44 | **11** | 78 |
| `mp show <10 MiB body>` | 76 | 73 | 146 |
| `mp search --local 'body:zolvertrix' -n 100` | 41 | **10** | 77 |
| `mp list` (drafts) | 42 | **9** | 39 |
| `mp list-messages --mailbox Bulk -n 5000` | 51 | 56 | 133 |
| `mp dump-mailbox --json -A alpha` | 86 | 121 | 199 |

A small answer costs 9-15 ms routed against 41-46 ms in process: about 33 ms saved, which is the config load plus store open the daemon now holds.
A multi-MiB answer costs about 35 ms more, so `mp dump-mailbox` is the one command that pays more than it saves; it is a batch export and it is in `BACKLOG.md`.
Cold is warm plus a flat 62-68 ms, which is one daemon start, paid once per daemon lifetime.
All ten workloads were checked byte-identical against the oracle before they were timed.

W1, W2's TUI half, W5, W6's cold-cache half and W8 stay `NOT TAKEN` in `docs/baselines/pre-daemon/measurements.md`, unchanged and still owner action.

### Follow-ups

Consolidated into `BACKLOG.md` with the ones P4-U6, U8, U10 and U12 left. This unit's own additions:

- The server leg of `mp search` (LST-06) is unmigrated and now has a name for what it needs (`message.search_server`).
- The two wizards need a wizard protocol before they can leave the client.
- `mp config show`'s three probes need a decision about whether a `config.*` method may probe a secret at all.
- `mp dump-mailbox --json` is slower routed than in process; it is the one command a streaming answer would help.
- No test drives two `mp` processes concurrently against one daemon, which is one of the six gate lines.
- The plan's "runtime assertion that the client process holds no `store.lock`" has no dedicated row; `tests/engine_lock_ingest_cli.rs` proves the property from the other side.

### Validation

`timeout 1800 cargo test --workspace --offline --no-fail-fast` -> **2 071 passed, 0 failed, 4 ignored**, across 43 result lines (2 068 + the three new boundary tests).
Each of the six slice suites green three times running: `daemon_read_slice` 22, `daemon_draft_slice` 34, `daemon_mutation_slice` 35, `daemon_sync_slice` 38, `daemon_send_slice` 50, `daemon_admin_slice` 44.
The help walk (`touch src/main.rs && cargo build --offline && MP=./target/debug/mp scripts/capture-cli-help.sh | diff - docs/baselines/pre-daemon/cli-help.txt`) is empty.
`timeout 900 cargo clippy --workspace --offline --all-targets` -> 39 warnings, against 39 at `4d8349e`: no new warning, and none on a file this unit touched.
`timeout 600 cargo build --offline` -> 0 warnings.
`timeout 900 cargo install --path . --offline` -> replaced.
`pgrep -af 'mp daemon'` empty after the full run and after the measurement run.
`git diff --stat 4d8349e..HEAD -- tests/` is `tests/architecture_boundaries.rs` only.
