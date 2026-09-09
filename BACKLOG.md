# Backlog

Index of open tickets. One file per item lives in [docs/tickets/](docs/tickets/); see [docs/tickets/README.md](docs/tickets/README.md) for the convention. Use the `ticket` fish function to add a new entry.

When a ticket is shipped: set `status: done` in the ticket file, add an entry to [CHANGELOG.md](CHANGELOG.md), and remove its line from this index.

## Now

> Architecture review 2026-08-06, follow-ups #0053 to #0064: [synthesis](.agents/handoff/2026-08-06_architecture-review-synthesis.md). Suggested order is #0053, #0054, #0055, #0056, then #0057 and #0058. #0053, #0054, #0055, #0056, #0057, #0058 and #0064 have shipped. Their post-ship reviews all passed and left deferred notes, which are filed as #0065 to #0071; #0065, #0066, #0067, #0068 and #0071 have shipped.

> Audit 2026-08-14, owner decisions across performance, UX/workflow and feature-survey: [synthesis](.agents/research/2026-08-14-audit-synthesis.md). Tickets #0087 to #0100 all shipped in 0.9.0 and after; #0101 is the only survivor, parked under Later.

> Preview latency on list navigation: [plan](docs/plans/preview-latency.md), four tickets in order, each measured before the next starts. All four have shipped: Ticket A ([#0108](docs/tickets/0108-coalesce-key-events.md), instrumentation and key-event coalescing), Ticket B ([#0109](docs/tickets/0109-retire-inline-image-rendering.md), retiring inline image rendering), Ticket C ([#0110](docs/tickets/0110-retire-auto-mark-read.md), retiring auto-mark-read) and Ticket D ([#0111](docs/tickets/0111-retire-rich-html-preview.md), retiring the rich HTML preview render). The measurement was waived and the cache-and-prefetch contingency closed as unnecessary.

> The fetch skip list that never converges: [plan](docs/plans/sync-skip-list.md), five tickets. [#0112](docs/tickets/0112-gate-message-id-rebind.md) is the one that stops the pain and has shipped, along with [#0117](docs/tickets/0117-rebind-gate-after-a-windowed-reset.md), the regression it left on the passes that finish a windowed UIDVALIDITY reset, and so has [#0116](docs/tickets/0116-one-outbox-drain-per-account.md), whose premise in the plan was wrong: the duplicate Sent copies were mp appending the same row from several concurrent drains, not Exchange filing its own copy, and so has [#0114](docs/tickets/0114-drain-mutation-queue-at-tail.md), which drains the mutation queue and the outbox at the tail of a tick as well as the head so a change made during a sync does not wait for the next one, and so has [#0113](docs/tickets/0113-bound-the-body-fetch.md), which chunks the body fetch and gives each mailbox a deadline so a slow one yields the tick instead of holding it, and so has [#0115](docs/tickets/0115-warn-on-a-non-converging-fetch.md), the detector that makes a fetch downloading the same messages pass after pass visible in the log, on the status line and in `mp sync`, written last so it was not tuned against the bug it would have caught. All five have shipped.

> Daemon-first architecture, on the way to a native GUI: [plan](.agents/workflow/native-gui-daemon/plan.md), eight tickets, one per phase, [#0118](docs/tickets/0118-pre-daemon-baselines-and-inventories.md) Phase 0 through #0125 Phase 6. The freeze is the `pre-daemon` tag (`f8af44b`) and the work lands on the `daemon` branch. #0118 has shipped: the committed baselines under [docs/baselines/pre-daemon/](docs/baselines/pre-daemon/), the 131-identifier [parity matrix](docs/parity-matrix.md), the engine-import boundary test, and the [gate evidence](docs/baselines/pre-daemon/gate.md). Two things carry out of it. Five measurement rows are `NOT TAKEN` on this host (preview p50/p95, cold first paint, mutation propagation, the TUI half of the 5000-row refetch, and the cold-cache CLI figure) and are owner action on a machine with a terminal and a real account, required before Phase 1a. And the Phase 0 gate line asking every GUI-parity capability for a target interaction is answered only by the deferral list below, because the plan designs the GUI in Phase 9; that half of the line moves to the Phase 9 checklist. [#0119](docs/tickets/0119-daemon-risk-spikes.md) Phase 1a has shipped too: the four decision records under [docs/baselines/decisions/](docs/baselines/decisions/) (keep JSON-RPC, whole-list transfer with row deltas, a temp-file handle above 1 MiB, two read connections per account), the bootstrap ordering Phase 3a implements ([docs/plans/daemon-bootstrap.md](docs/plans/daemon-bootstrap.md)), and the dependency due diligence ([docs/plans/daemon-dependencies.md](docs/plans/daemon-dependencies.md)), which adopts nothing new. Two things carry out of it. `spikes/ipc-bench` is kept rather than deleted at the gate, because Phase 6 re-runs its workloads against the complete dispatcher and a rewritten benchmark is not the same benchmark; the deletion moves to P6-U10. And the two interactive numbers, the held-key preview A/B and cold first paint, are still owner action, unchanged from #0118.
> [#0120](docs/tickets/0120-daemon-transport-and-read-only-methods.md) Phase 2 has shipped, and it is the first one with product code in it: the workspace, `crates/mp-protocol` and `crates/mp-client`, the runtime files and the start lock, the `mp daemon` lifecycle commands, the `initialize` handshake, and the read-only `account.list` and `message.list` behind a hidden `--daemon`.
> All of it is behind the `daemon` cargo feature, so `cargo install --path .` ships an unchanged `mp` and `mp --help` is byte-identical to the pre-daemon baseline in both builds; the gate evidence is [docs/baselines/phase2-gate-evidence.md](docs/baselines/phase2-gate-evidence.md), the wire contract [docs/daemon-protocol.md](docs/daemon-protocol.md), and the operator's half [docs/daemon-operations.md](docs/daemon-operations.md).
> Two gate lines are not fully signed off here: the macOS half of "crash and stale-socket recovery pass on macOS and Linux" is escalated, since this is a Linux host, and the `config.*` half of the zero-config line moves to Phase 3b, which is where the plan puts that method family.
> Three follow-ups carry out of it, none of them blocking Phase 3a.
> The `account.list` and `message.list` fixtures are checked for canonical form, round-tripping and their documented field sets, but not against a live daemon's answer, where `initialize` is; the missing test is the read-only twin of `initialize_request_matches_the_pinned_fixture_shape`.
> The per-call timeout lives in `src/main.rs` around each `Connection::call`, so `mp-client` itself has none and a second consumer would have to reinvent it.
> And `mp-client` skips server-initiated notifications, because delivering `state.event` and `state.resync_required` needs an owned reader task rather than a borrowed one, which is Phase 3a's to build.
> [#0121](docs/tickets/0121-daemon-dispatcher-and-state-model.md) Phase 3a has shipped: the dispatcher every method now registers on, the canonical state with its revision counter and `state.bootstrap`, per-connection event delivery with coalescing and backpressure, the operation registry with `operation.status` and `operation.cancel`, and `mailbox.list` as the third read-only method.
> Still all behind the `daemon` feature, still byte-identical help in both builds; the gate evidence is [docs/baselines/phase3a-gate-evidence.md](docs/baselines/phase3a-gate-evidence.md), and the seven deviations are in the ticket rather than left to a diff.
> Six follow-ups carry out of it, none of them blocking Phase 3b; the double store open in `mailbox.list` and the clippy warnings in the T-unit test files were closed by the review-fix commit that followed the sweep.
> The bootstrap snapshot's `operations` projection is unit-tested against the registry but never taken over a socket while an operation is in flight, so the one shape a reconnecting GUI depends on is the one no end-to-end test covers.
> `Outbound::rebootstrap` clears the whole queue, so the lifecycle events an overflow deliberately keeps are discarded by the re-bootstrap that follows it: either the second bootstrap should keep them or the overflow should not.
> `daemon.status` reports no per-connection queue depth, which is the diagnostic an operator wants when a client falls behind and the reason bounded memory had to be pinned in process instead of over the wire.
> `EventQueue::try_recv`, `len`, `is_empty` and, since `drain_all` took over the connection loop, `drain` and `drain_lifecycle` are called only by the contract tests, and `Change::kind()` maps three kinds (`mailbox.counts_changed`, `draft.removed`, `outbox.counts_changed`) that no longer travel since `Event::from_change` supersedes them; both are for the first phase that touches those files, not a drive-by deletion.
> `mp-client`'s `StateTracker` treats any revision above `watermark + 1` as a gap, which coalescing makes wrong the moment a real method commits two mergeable changes: either the daemon carries the highest revision a merged entry superseded, or the client stops doing arithmetic and trusts `state.resync_required`.
> The per-call timeout and the fixtures-against-a-live-daemon gap carried from #0120 are both still open, and `mp-client` now does deliver notifications, which was the third of them.

Settled deferrals for the daemon migration, recorded here because the Phase 0 gate points at this list:

- The first GUI release is dark-only. The light theme is deferred, which is the one `deferred` status in the parity matrix (`OBS-07`); `theme` stays a top-level `config.toml` key read once at startup until then.
- `mp fetch` (`LST-11`) keeps its command surface through the migration. Whether to deprecate it in favour of `sync` plus `search` is deferred to its own ticket rather than decided inside the cutover (`ANO-3`).
- `schemars` is not adopted now. Phase 2 pins the protocol with checked-in JSON fixtures, and the JSON Schema question reopens at Phase 7, the first consumer.
- `notify` is not adopted. The daemon's draft, signature and config watcher uses the 1-second fingerprint poll the TUI already runs, moved into the daemon, and a filesystem-notification crate is revisited only if that poll proves inadequate under test.
- `uuid` stays transitive. Ids are minted from `rand` plus a hex format, so no RFC-4122 shape and no new direct dependency.
- `SyncResult.bodies_truncated` stays a count, and so does the `bodies_truncated` field of the daemon's `sync.completed` payload, which P3b-U6 shipped carrying the data that exists. Widening either to the list of deadline-stopped mailbox names is a behaviour change to the engine and to the wording contracts of #0113 and #0115, it moves a wire field from `u64` to an array and therefore needs a protocol-changelog entry, and it gets its own ticket rather than riding along with the daemon's sync event.
- The capabilities #0109, #0110 and #0111 retired (inline images, auto-mark-read, the rich HTML preview) are not restored by the GUI. Their identifiers stay reserved in the matrix so a later document cannot rebind them.

## Next

> Data-access-layer redesign (DECIDED 2026-07-14, decisions settled 2026-07-31): server-as-truth SQLite mirror + content-addressed blob store; drafts local-only, received read-only. Greenfield rebuild on a branch, no dual-write, safety net is `mp-legacy` + the `pre-dal-nuke` tag. Plan: [docs/plans/data-access-layer.md](docs/plans/data-access-layer.md). Stage 0 (#0049, the pre-nuke oracle capture and the `pre-dal-nuke` freeze) is done. Order below is the build order; the stop-gate sits after the #0038 + #0050 + #0052 triple, because the product is only half usable between them. #0038, #0050 and #0052 have all shipped, so the stop-gate is reached and the stages below it are the work after the pause.

## Later

> TUI multi-view roadmap: [docs/plans/tui-restructure-views.md](docs/plans/tui-restructure-views.md). All three views have shipped: foundation (#0032), view switcher + Contacts (#0033), local calendar (#0034).

- [#0081 QRESYNC, UIDPLUS, and advancing the modseq on a capped pass](docs/tickets/0081-qresync-uidplus.md) -- perf _(the split-out half of #0041, which shipped the session pool and the CONDSTORE delta)_
- [#0085 On-open re-fetch of an evicted body](docs/tickets/0085-on-open-body-refetch.md) -- feature _(the missing half of #0060, whose eviction sweep shipped; required before lowering a cap below the working set)_
- [#0101 Conversation-view collapse and inline navigation on top of the thread view](docs/tickets/0101-conversation-view-collapse-inline-nav.md) -- feature _(cross-ref #0008)_
- [#0084 iMIP send-side updates and cancellations](docs/tickets/0084-imip-send-cancel-and-update.md) -- feature _(the split-out send half of #0031, whose receive half shipped)_

### Distribution / cross-platform (adoption track)

> Windows is targeted via WSL only. Native Windows (msvc, Credential Manager, Scoop, winget, EV signing) is out of scope.

- [#0012 Apple Developer ID signing for macOS releases](docs/tickets/0012-apple-developer-id-signing.md) -- chore
- [#0014 Linux packaging (.deb, .rpm, AUR, musl)](docs/tickets/0014-linux-packaging.md) -- chore
- [#0015 Cross-platform smoke tests](docs/tickets/0015-cross-platform-smoke-tests.md) -- chore

## Parked (Graph)

> Graph backend parked (decision 2026-08-06): nothing depends on it today, and the priority is features and stability on the IMAP/SMTP path.
> It wakes deliberately, not opportunistically: the first live target is the EVOQS Exchange account, the items below are picked up together, and the pre-live-contact checklist (end of [0065-followup-report](.agents/workflow/0065-followup-report.md), also the Verification section of [#0065](docs/tickets/0065-graph-prune-batch-hardening.md)) is run before that first contact.

- [#0035 Graph API admin approval + Azure app verification](docs/tickets/0035-graph-admin-approval.md) -- chore _(blocked; written against the TUM tenant, re-scope for EVOQS on wake)_
- [#0036 Graph sync backend (calendar + server-side RSVP)](docs/tickets/0036-graph-sync-backend.md) -- feature _(blocked by #0035)_
- [#0082 Verify the Graph delta against a live tenant](docs/tickets/0082-graph-delta-live-verification.md) -- perf _(the split-out half of #0042, which shipped the `/messages/delta` path and its fallbacks but had no Graph account to smoke them against)_
- [#0063 Send durability gaps, Graph half](docs/tickets/0063-send-durability-gaps.md) -- bug _(the SMTP halves shipped; scope item 3, resumable Graph `pending_send` rows, waits with the backend)_

The parity half of [#0059](docs/tickets/0059-syncbackend-trait.md) is parked with it: the trait and the engine shipped, but `graph.rs` still runs its own loop rather than being a second `SyncBackend`.
