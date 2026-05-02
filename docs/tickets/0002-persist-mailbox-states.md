---
id: 0002
title: Persist mailbox_states across TUI sessions
type: perf
priority: now
status: done
created: 2026-05-01
---

The 16 s "first quick sync after launch" is **diagnosed**: `prev_states` (`AccountState.mailbox_states`) starts empty on TUI launch, so the reconcile decision in `sync_mailboxes` (`src/imap_client/sync.rs:319`) falls through to the conservative `true` branch and runs a full Message-ID scan of INBOX + Archive (~14 s on xmail.mwn.de). Subsequent syncs in the same session skip reconcile correctly (1.2 s) -- 13x speed-up confirmed.

See [.claude/research/2026-04-29_sync-timing-analysis.md](../../.claude/research/2026-04-29_sync-timing-analysis.md).

## Fix

Save `mailbox_states` to `account_dir(name)/mailbox_states.json` on quit / after each sync; reload in `AccountState::new`. Validate `uid_validity` on next SELECT; fall back to full reconcile if it changed (handles other-client mutations safely).

This eliminates the 14 s first-sync penalty entirely.

## Acceptance

- First sync after a fresh TUI launch is <2 s on TUM (warm cache).
- `uid_validity` mismatch falls back to full reconcile without crashing.
- `[TIMING]` logs show the reconcile-skip branch is taken on cold start.

## Related

- Already done: `[TIMING]` instrumentation in `open_imap_session`, `sync_mailboxes`, `fetch_new_emails_on_session`, `lib_do_sync`, `AccountState::new`.
- Logs at `<mailypoppins_data_dir>/logs/mailypoppins-YYYY-MM-DD.log`.
- Filter with `rg '\[TIMING\]'`.

## Resolution (2026-05-02)

- `MailboxState` now derives `Serialize`/`Deserialize`.
- New helpers in `src/imap_client/sync.rs`:
  `mailbox_states_cache_path`, `load_mailbox_states_cache`,
  `save_mailbox_states_cache`. Cache file is
  `<account_dir>/mailbox-states.json` (kebab-case, matching
  `contacts-cache.json`).
- `AccountState::new` populates `mailbox_states` from the cache; missing
  / corrupt files degrade silently to an empty map (worst case: one
  extra full reconcile).
- `tui/bg.rs` writes the cache after every successful Fetch / Sync that
  returned new states. Best-effort; failures only log.
- `uid_validity` mismatch falls back to full reconcile via the existing
  `MailboxState::needs_reconciliation` logic, so other-client moves /
  mailbox renumbering remain safe.
- Tests: 6 new unit tests covering path resolution, missing-file
  fallback, round-trip, parent-dir creation, corrupt-file degradation,
  and the reconcile-skip happy path. 292 unit tests pass.
