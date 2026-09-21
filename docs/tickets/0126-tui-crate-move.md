---
id: 0126
title: The crate boundary for the TUI, P5-U10a/b/c
type: refactor
priority: now
status: in progress
created: 2026-09-22
---

Status: in progress. P5-U10a and P5-U10b have landed: `crates/mp-core` holds the engine-free closure the TUI reaches, eleven modules whole and the engine-free half of six more.
No call site outside the moved files changed, and no behaviour changed: the help surface, the key dump and the twenty golden frames are byte-identical, and the workspace test count did not drop.
**P5-U10b landed its splits and not its surfaces**: `RD-06`, `RD-07`, `LST-08`, `LST-09` and the wire-row types are unbuilt, so the allow-list is ten rows still and P5-U10c carries them along with the move.

Ninth ticket of the daemon-first architecture plan (`.agents/workflow/native-gui-daemon/plan.md` section 3.7), carrying the unit the plan wrote as one and [#0124](0124-tui-cutover.md) found to be three.

The plan's P5-U10 is `git mv src/tui crates/mp-tui/src` plus a dependency line.
#0124 measured what stands in the way and wrote it down: the obstacle is not the engine residue the allow-list records, it is the fourteen *shared* modules `src/tui/` reaches, which the allow-list deliberately does not scan, and whose own closure is about 15 000 lines across sixteen modules.
Six of them need a genuine split, and none of it is new production code, which is why a line budget did not catch it.
That ticket's proposed sequencing is this one's unit table.

## The unit table

| unit | kind | commit | subject | status |
|---|---|---|---|---|
| P5-U10a | I | `0d712c7`, `11370eb`, `9311bef`, `84385b1`, `e9a2773` | the shared crate: `crates/mp-core`, the eleven-module engine-free closure plus the `selector`, `search` and `invite` splits | done |
| P5-U10b | I | `3a93341`, `3a2dc58`, `e88eb89`, `de575de`, `25a6ee6` | the three remaining splits (`reconcile`, `contacts`, `draft`), the `addresses` move out of `send`, and the draft body through `draft.path` | done, partially: the four surfaces did not land |
| P5-U10c | I | - | `RD-06`, `RD-07`, `LST-08`, `LST-09`, the wire-row types, and then the move itself: `git mv src/tui crates/mp-tui/src`, the test modules that link the engine, and the P2-U1a guard's scan roots | pending |

## P5-U10a: the shared crate

### What moved

Eleven modules moved whole, and three moved in halves. Line counts are the files as they stand after the move.

| module | lines | destination | split seam |
|---|---:|---|---|
| `app_state` | 174 | `crates/mp-core/src/app_state.rs` | - |
| `calendar` | 571 | `crates/mp-core/src/calendar.rs` | - |
| `config` | 2 657 | `crates/mp-core/src/config.rs` | - |
| `notify` | 280 | `crates/mp-core/src/notify.rs` | - |
| `oauth2` | 495 | `crates/mp-core/src/oauth2.rs` | - |
| `parse` | 1 952 | `crates/mp-core/src/parse.rs` | - |
| `secrets` | 718 | `crates/mp-core/src/secrets.rs` | - |
| `signatures` | 1 002 | `crates/mp-core/src/signatures.rs` | - |
| `sync_health` | 306 | `crates/mp-core/src/sync_health.rs` | - |
| `timing` | 131 | `crates/mp-core/src/timing.rs` | - |
| `types` | 558 | `crates/mp-core/src/types.rs` | - |
| `selector` | 596 + 79 | `crates/mp-core/src/selector.rs` + `src/selector.rs` | after `parse_in`: the grammar moves, `resolve_received` / `resolve_draft` stay |
| `search` | 1 445 | `crates/mp-core/src/search.rs` | none; it moved whole once its three helpers could come with it |
| `imap_query` | 126 | `crates/mp-core/src/imap_query.rs` | three functions out of `src/imap_client/search.rs`, which keeps the rest and re-exports them |
| `invite` | 891 + 139 | `crates/mp-core/src/invite.rs` + `src/invite.rs` | at `GRAPH_REFUSAL`: the iMIP half moves, `InviteRequest` / `InvitePlan` / `plan_invite` stay |

The eleven whole modules are 8 844 lines as they stand (7 237 at the ticket's estimate of 7 000, before the `test-support` gating).

### The strongly connected component

`{config, oauth2, secrets, signatures, app_state}` is one cycle and had to move in one commit: `config` dispatches the secrets backend and resolves a signature, `oauth2` reads `config::tokens_dir` and `secrets`' blob encryption, `secrets` reads `config::config_dir`, `signatures` reads `config` and `app_state`, and `app_state` reads `config::mailypoppins_data_dir`.
No ordering of five `git mv` calls leaves a compiling tree, so the first commit is the whole component plus the six modules that hang off it.

### The three splits, and where the seam sits

**`selector`.** Three `use crate::store::…` lines were the whole problem, and both users of them are below `parse_in`.
The grammar, the parser, the formatter, the percent-encoding and `draft_not_found` moved with the 11 tests, which are all pure; `resolve_received` and `resolve_draft` stayed beside the store.

The one thing that did not fall out was `Selector::for_message`, which took a `&MessageRow`: an inherent method can only be written in the crate that defines the type, and the row is a store type.
It is generic over a new `mp_core::selector::MessageRowRef` trait now, two accessors, implemented for `store::read::MessageRow` in the root crate, with a blanket impl over `&T` for the call sites that pass a double reference.
All seven call sites are unchanged, because inference picks `R = MessageRow` at each of them.
`not_found`, `ambiguous` and `message_key` are `pub` + `#[doc(hidden)]` for the two resolvers that stayed.

**`search`.** Nothing of it is engine-side; its only engine dependency was three pure string functions it imports from `imap_client::search` (`normalize_message_id`, `bracketed_message_id`, `parse_date_to_imap`), which live there because that is where the first caller was.
The brief allowed moving them or duplicating them, whichever is smaller: they moved, into `mp_core::imap_query`, because duplicating them would be two answers to "what is the wire form of a Message-ID".
`imap_client::search` re-exports all three, so the `use super::*` in its own tests, the `imap_client` re-export and every call site elsewhere name the same functions.
Seven tests moved with the three helpers; the other fourteen in that module stayed with the code they test.

**`invite`.** One function reads an account: `plan_invite` refuses Graph by the auth method, takes the `ORGANIZER` from `default_from` and splits recipients with `send::split_addresses`.
The seam is `GRAPH_REFUSAL`, which only `plan_invite` uses.
Everything above it is iMIP and moved, with all 18 tests: none of them plans an invite.
The TUI's only use of this module is `Rsvp`, which is now where `crates/mp-tui` will be able to read it.

### The re-export shape

`src/lib.rs` carries one `pub use mp_core::{app_state, calendar, config, notify, oauth2, parse, search, secrets, signatures, sync_health, timing, types};` for the modules that moved whole, and the three split modules stay `pub mod` files whose first statement is `pub use mp_core::<module>::*;`.
`crate::config::…`, `mailypoppins::parse::…` and every other old path therefore resolve unchanged in the CLI, the daemon, the TUI and the integration tests.

The proof is mechanical: `git diff 062fc8d..HEAD --stat -- src tests crates/mp-protocol crates/mp-client` is the sixteen deleted or halved moved files, `src/lib.rs` (21 lines), `src/imap_client/search.rs` (the re-export seam) and `tests/test_selection_guard.rs` (the floor). Nothing else under those four paths moved a byte.

Seven `pub(crate)` items crossed the boundary and are `pub`: `parse::{floor_char_boundary, ensure_utf8_charset, inject_csp_meta, sanitize_attachment_filename}`, `types::collapse_hyphens` and `config::{validate_retention, reject_legacy_keys}`.

### `#[cfg(test)]` does not cross a crate boundary

The one genuine hazard of the move, and the reason it is not a pure `git mv`.
Two of the moved modules carry test seams the root crate's own tests depend on:

- `config::test_env`, the thread-local `$HOME` / `$MAILYPOPPINS_DATA_DIR` / `$MAILYPOPPINS_CONFIG_DIR` overrides #0077 introduced to stop tests racing on `environ`, read by `home_dir`, `config_dir_env` and `mailypoppins_data_dir`;
- `parse::test_temp_root`, the per-thread materialisation root `materialisation_root` returns under test.

A dependency is compiled without `cfg(test)`, so both would have silently evaluated to their production branch the moment these modules stopped being files in the root crate, and every `TestDataDir` in the tree would have pointed at the real `$HOME`.
Nothing would have failed to compile.

They are behind a `test-support` feature instead, spelled `#[cfg(any(test, feature = "test-support"))]` so `cargo test -p mp-core` still has them, and the root crate turns the feature on through a second `mp-core` entry in `[dev-dependencies]`.
Resolver v2 (edition 2021) does not unify a dev-dependency's features into a build that does not build dev targets, which is the property that makes this exactly equivalent to what `#[cfg(test)]` did: verified with `cargo build -v`, where `mp_core` is compiled with no `--cfg feature="test-support"`, against `cargo test -v`, where it is.
`test_env` and `test_temp_root` are `pub` + `#[doc(hidden)]` behind that feature, and `tempfile` is an optional dependency the feature turns on.

### The guard and its floor

`tests/test_selection_guard.rs` gains a fourth floor over `crates/mp-core/src`, `MIN_CORE_TESTS = 329`, and one test that asserts it.
This is the unit's one test edit, and it is the guard doing what it was written for: 329 tests left `src/` in one commit range, and a workspace layout that deselected them would otherwise only have shrunk a summary line.

The arithmetic, which is the move's own proof:

| | root `--lib` | `mp-core` | sum |
|---|---:|---:|---:|
| at `062fc8d` | 1 398 | - | 1 398 |
| after the eleven modules | 1 145 | 253 | 1 398 |
| after the `selector` split | 1 134 | 264 | 1 398 |
| after the `search` split | 1 087 | 311 | 1 398 |
| after the `invite` split | 1 069 | 329 | 1 398 |

329 = 253 (eleven modules) + 11 (`selector`) + 40 (`search`) + 7 (`imap_query`) + 18 (`invite`).

`tests/architecture_boundaries.rs` needed no change: its scan roots are `src/tui/` and the client sources, neither of which moved, and the allow-list is unchanged at ten rows.

### Deviations

**`ENGINE_MODULES` now names two modules that live in `mp-core`.** `secrets` and `oauth2` are on that list and are in the shared crate.
No file under `src/tui/` imports either, so the allow-list did not move, and re-classifying them would be a test edit this unit was not asked to make.
It is a hole to close in P5-U10c: a `crates/mp-tui` depending on `mp-core` could reach both, and the scan (which looks for `use crate::` / `use mailypoppins::`) would not see it.

**Two clippy warnings the boundary invented, and two `#[allow]`s.** The baseline is 38 distinct warnings and the move must not add one; it added `new_without_default` on `test_env::TestDataDir::new` (which fires only because the type became `pub`) and `items_after_test_module` on a `mod tests` the two signature loaders have always followed (which the lint started seeing when `test_env` above it stopped being a plain `#[cfg(test)] mod`).
Both are `#[allow]`ed where they fire with the reason; the count is back to 38 and the six warnings now reported against `crates/mp-core` are the same six that were reported against `src/config.rs`, `src/oauth2.rs` and `src/parse.rs` before.

**No `rustfmt` run.** Every moved file except the two `lib.rs` headers and the new `src/selector.rs` was already rustfmt-dirty at `062fc8d`, checked file by file, so formatting them would have buried the move in reflow.

**Four intra-doc links became code spans.** `crate::store::drafts::SkippedDraft`, `crate::draft::validate_draft`, `crate::contacts::cache` and `crate::imap_client::search` name modules that are no longer in the same crate, so the four rustdoc links to them are plain code spans now. No prose changed, and `cargo doc -p mp-core` reports no broken link.

### Follow-ups

- P5-U10b splits `reconcile`, `contacts` and `draft`, which are the three modules of the closure that this unit did not touch.
- `ENGINE_MODULES` and `mp_core::` above: P5-U10c decides whether `secrets` and `oauth2` leave the list or the scan learns about the new crate path.
- `mp-core` is one crate for what may later want to be two: the configuration and secrets half is a client's, and the RFC822 parsing half is arguably the engine's. Nothing needs it split today, and a second boundary drawn before `crates/mp-tui` exists would be drawn blind.

### Validation

`TMPDIR=/var/tmp cargo test --workspace --offline` -> **2 347 passed, 0 failed, 5 ignored**, across 56 binaries.
That is #0125's 2 346 plus the guard's new `core_test_count_has_not_dropped`, and nothing else: no test was added and none was lost.
Per crate: `mailypoppins` lib 1 069, `mp` bin 2, `mp-core` 329, `mp-protocol` 14, `mp-client` 7, the 47 integration binaries 925, and one `mp_client` doc test.

`--test phase5_parity_gate` -> 11. `--test test_selection_guard` -> 6. `--test architecture_boundaries` -> 6, the allow-list unmoved at ten rows.
`--lib 'ui::golden_frames::'` -> 20 and `--lib golden_frames_daemon` -> 22, with no snapshot re-approved and no `.snap.new` left behind.

`scripts/capture-cli-help.sh` and `mp dump-keys --json`, from a binary rebuilt in the same run, diff empty against `docs/baselines/pre-daemon/cli-help.txt` and `docs/baselines/pre-daemon/tui-keys.json`.

`cargo clippy --workspace --offline --all-targets` -> **38 distinct warnings**, the same 38 as `062fc8d` by `(lint, file, line)`, with the six that moved reported against their new paths.

`cargo install --path . --offline` -> replaced, release profile, 31 s.
`pgrep -af '[m]p daemon'` showed one pid throughout, the owner's long-running daemon, which no run touched.

## P5-U10b: the three remaining splits

This unit was briefed as two halves: split `reconcile`, `contacts` and `draft` on the way past, and build the four surfaces (`RD-06`, `RD-07`, `LST-08`, `LST-09`) plus the wire-row types that drive `tests/fixtures/tui-engine-imports.txt` to zero.
It landed the first half whole and one of the follow-ups; the second half did not land, and the allow-list is ten rows still.
That is written down here rather than rediscovered, in the same spirit as #0124's measurement of why P5-U10 was three units and not one.

### What moved

| module | destination | split seam |
|---|---|---|
| `reconcile` | `crates/mp-core/src/reconcile.rs` + `src/reconcile.rs` | after `ReconcileReport`: the fold moves, the three functions that open a store stay |
| `contacts` | `crates/mp-core/src/contacts/` (7 files) + `src/contacts/` (2) | `cache`, `filter`, `matcher`, `rank`, `types`, `vcard` whole; `extractor` split at the store read; `hooks` stays |
| `draft` | `crates/mp-core/src/draft.rs` + `src/draft.rs` | the file format moves, five operations that need an index, a row or an outbox record stay |
| `addresses` | `crates/mp-core/src/addresses.rs` | four pure functions out of `src/send.rs`, which re-exports the three public ones |

**`reconcile`.** The seam is exact and the module docs already drew it: `load_invites` and `invite_from_row` read blobs, `event_for_message` and `reconcile_account` call them, and everything above is a fold over an `&[InviteMessage]` a caller already holds.
No test moved, because every test in the module is seeded through the real ingest path and belongs with the store half.

**`contacts`.** Six of the nine files import nothing engine-side and moved untouched.
`extractor` split at the store read: `build_index_for_account` and `build_index_from_store` stay, and the observation half (`ObservedIn`, `observe`, `process_header`, `self_address`, `empty_index`, `parse_date_to_rfc3339`, `UNDATED_OBSERVED_AT`) moved with the six `observe_*` tests, with five items `pub` + `#[doc(hidden)]` for the rebuild that still calls them.
`hooks` stays whole: it takes a `sync::FreshObservation`.

`src/contacts/mod.rs` names its fifteen re-exports one by one rather than globbing.
A `pub use mp_core::contacts::*;` re-exports `mp_core`'s `extractor` module, and the root's private `mod extractor` then shadows it, which is `hidden_glob_reexports` and a warning the baseline does not carry.

**`draft`.** The largest of the three, 3 028 lines with 54 tests in one block in the middle of the file.
A draft is a file, and the file format is all of it bar five operations: `new_draft_skeleton` (mints an id through the drafts index), `source_from_row` and `account_name_of` (read a store row), `create_draft_from_source` (mints, reindexes and names a selector), `settle_sent_draft` (reads an outbox record) and `delete_indexed_draft`.
45 tests moved; the nine that stayed are the two skeleton rows, the two delete rows and the five settle rows with their three `send::SendReport` fixtures.
`draft_with_unknown_fields` is duplicated into the root's test module, because a `#[cfg(test)]` helper does not cross a crate boundary and the settle rows need it.

**`addresses`.** `validate_draft`, `create_reply_draft_from` and `create_forward_draft_from` are the three functions that made `draft` look engine-bound, and their only engine reference was `send::split_addresses` / `send::normalize_address_for_smtp`: pure string functions that live in `send` because that is where the first caller was.
The `imap_query` precedent of P5-U10a applies exactly, so they moved with `quote_display_name` and `format_recipient` and `send` re-exports the three public ones.
Twelve of the eighteen tests moved; the six that parse the result with `lettre::message::Mailbox` stayed, because what they assert is that the transport accepts the output, which is `send`'s contract rather than the string's.

### The draft body, the fourth residue site

#0124's follow-ups named `App::load_draft_body` as a fourth site with the shape of the three `TUI_APP_STORE_RESIDUE` names, invisible to that table's scan because it opens the store with `Store::open` rather than `open_store` (a drafts-only account has no store *file* and still has drafts).
`App::draft_body` routes it the way `message_body` and the three invitation readers are routed: `draft.path` on the session the `App` holds, and the file it names parsed here with `mp_core::draft::parse_email_draft`.
The body does not travel. A draft is a local Markdown file whose format both ends of the socket read with the same parser, so shipping the body would be a second answer to "what is the body of this file" and a second place for the signature sentinels to be stripped; what the client cannot do without the daemon is turn an `id:` into a path, which is the index read `draft.path` already served.
`load_draft_body` stays as the sessionless oracle, and `src/tui/app/invites_tests.rs` gains two equality rows (8 -> 10).
No new method, no new protocol type, no new fixture: `draft.path` is P4-U6's and `DraftLocation` is already in `crates/mp-protocol/src/draft.rs`.

### What did not land, and why

The four surfaces and the wire-row types.
The unit's own splits are 5 400 lines of moved code across four commits, and each of the four surfaces is a daemon method with a fixture pair, a `tests/daemon_*_slice.rs` row, a `docs/daemon-protocol.md` entry and a client decoder before it removes anything.
Measured against the tree rather than against the brief, the four are not one unit's worth of work:

| row | the surface it wants | the size of it |
|---|---|---|
| `actions.rs store` | `RD-06` `message.markdown`, `RD-07` a selector the listing carries, `LST-09` `message.fetch` | three methods, and the row also has two `#[cfg(test)] use crate::store::…` in `actions.rs` itself that no method removes |
| `helpers.rs store`, `helpers.rs imap_client`, `mod.rs store` | `LST-08` `message.list_server` | the server search leg, which is also `LST-06`'s unmigrated `CLI_ENGINE_RESIDUE` group: one method retires both |
| `app/types.rs store`, `app/types.rs ingest`, `queries.rs store` | `MessageRow` / `DraftRow` / `SkippedDraft` as wire rows | `entry_from_row`'s signature, plus `indexed_drafts` (a sessionless store reader) and ~700 lines of store-backed tests in `types.rs` |
| `app/mod.rs store`, `app/calendar_view.rs store`, `app/store_rows.rs store` | nothing; they are the sessionless oracles | they die with the crate move, not with a method |

The honest count is that the last group cannot go before P5-U10c and the third group cannot go without it either, because `app/types.rs ingest` is a test module and `app/types.rs store`'s `indexed_drafts` is an oracle of the same kind.
So the allow-list's floor before the move is four rows rather than zero, and P5-U10c is not a pure `git mv` under any sequencing.
P5-U10c's brief therefore carries the four surfaces, the wire rows and the move.

### Deviations

**`mp-core` gained `lettre`.** `draft::validate_draft` checks an address by parsing it with `lettre::message::Mailbox`, which is what makes it a validator of "an address the send path will accept" rather than a second regex.
The entry is `default-features = false, features = ["builder"]`, so no transport and no TLS stack is asked for; cargo's workspace feature unification still builds the root crate's transports, which is a property of the build and not of the dependency graph.
`gray_matter`, `walkdir` and `nucleo-matcher` came with `draft` and `contacts::matcher` and want no comment.

**One clippy warning went away.** The baseline is 38 distinct `(lint, file, line)` and the tree reports 37.
`src/draft.rs` had `mark_as_approved` and `mark_as_draft` after its `mod tests`, which is `clippy::items_after_test_module`; both moved into `crates/mp-core/src/draft.rs` above its test module, so the lint stopped firing. Nothing was added.

**No `rustfmt` run on the moved files.** `src/draft.rs` was 30 rustfmt-dirty hunks at `b7da5d5` and the two halves carry 23 and 7; `src/reconcile.rs` was 19 and carries 18 + 1; `src/send.rs` was 37 and is 37.
Formatting any of them would have buried the move in reflow, which is P5-U10a's reasoning unchanged.
The files this unit wrote (`crates/mp-core/src/contacts/mod.rs`, `src/contacts/mod.rs`, `crates/mp-core/src/addresses.rs`, the two `queries.rs` / `invites_tests.rs` additions) are rustfmt-clean.

### The guard and its floor

`MIN_CORE_TESTS` 329 -> 416, with the arithmetic in the constant's doc comment.
416 = 329 (P5-U10a) + 30 (`contacts`) + 12 (`addresses`) + 45 (`draft`) + 0 (`reconcile`).

| | root `--lib` | `mp-core` | sum |
|---|---:|---:|---:|
| at `b7da5d5` | 1 069 | 329 | 1 398 |
| after `reconcile` | 1 069 | 329 | 1 398 |
| after `contacts` | 1 039 | 359 | 1 398 |
| after `addresses` | 1 027 | 371 | 1 398 |
| after `draft` | 982 | 416 | 1 398 |

`tests/architecture_boundaries.rs` needed no change: none of the moved files is under `src/tui/`, and the allow-list is unchanged at ten rows.

### Follow-ups

- The four surfaces and the wire rows, now P5-U10c's, with the floor-of-four caveat above.
- `ENGINE_MODULES` still names `secrets` and `oauth2`, which live in `mp-core`; P5-U10a raised it and P5-U10c still has to decide whether they leave the list or the scan learns about `mp_core::`.
- `crate::contacts::build_index_for_account` and `crate::draft::{create_draft_from_source, source_from_row, settle_sent_draft}` are store-half functions the TUI calls, so `crates/mp-tui` cannot compile against `mp-core` alone until each is a daemon method or a call the client stops making. `draft.create`, `draft.reply`, `draft.forward` and `contact.rebuild` are all served; what is left is routing the call sites, which is P5-U10c's or a unit after it.
- `mp-core` now carries `lettre`, `gray_matter`, `walkdir` and `nucleo-matcher`. It was already one crate for what may want to be two, and the draft/contacts half is the part a GUI would rather not link; nothing needs it split today.

### Validation

`TMPDIR=/var/tmp cargo test --workspace --offline` -> **2 347 passed, 0 failed, 5 ignored**, the count at `b7da5d5` exactly: no test was added by the splits and none was lost. Per crate after the unit: `mailypoppins` lib 984 (982 plus the two draft-body rows), `mp-core` 416, `mp-protocol` 14, `mp-client` 7, `mp` bin 2, the integration binaries and the doc test unchanged.

`--test phase5_parity_gate` -> 11. `--test architecture_boundaries` -> 6, the allow-list unmoved at ten rows. `--test test_selection_guard` -> 6. `--test daemon_protocol_fixtures` unchanged, since no fixture moved.
`--lib 'ui::golden_frames::'` -> 20 and `--lib golden_frames_daemon` -> 22, no snapshot re-approved and no `.snap.new`.

`scripts/capture-cli-help.sh` and `mp dump-keys --json`, from a binary rebuilt in the same run, diff empty against `docs/baselines/pre-daemon/cli-help.txt` and `docs/baselines/pre-daemon/tui-keys.json`.

`cargo clippy --workspace --offline --all-targets` -> **37 distinct warnings**, the baseline's 38 minus the `items_after_test_module` above, none of them on a line this unit wrote.
