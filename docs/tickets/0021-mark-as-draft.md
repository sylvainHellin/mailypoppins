---
id: 0021
title: mark as draft
type: feature
priority: now
status: done
created: 2026-05-01
---

We currently have `A` for marked as approved, and I would like to have a similar one for marked as draft (sometimes I mark as approved by mistake and want to reverse)

## Resolution (2026-05-03)

- New `mark_as_draft(path)` in `src/draft.rs`, mirror of
  `mark_as_approved`. Accepts only `Approved` (demoted to `Draft`),
  is a no-op on `Draft` (`"Already a draft: ..."`), and rejects
  `Sent` / `Inbox` / `Archived` with descriptive errors so we never
  silently rewrite a synced server email's status.
- TUI hotkey `D` in the email list -- chosen as the uppercase mirror
  of `A` (Approve), matching the existing `a`/`A` (archive/approve)
  case-shift convention. Single-email path fires `Action::MarkDraft`
  directly (non-destructive, fully reversible by `A`); multi-select
  goes through a confirm dialog as `Action::BatchMarkDraft(paths)`,
  matching `BatchApprove`.
- New `Action::MarkDraft` / `Action::BatchMarkDraft(Vec<PathBuf>)`
  variants and `ConfirmAction::MarkDraft`. Handlers in
  `src/tui/actions.rs` reload the current mailbox so the status badge
  updates immediately.
- CLI: `email mark-draft <file>` subcommand for parity with
  `mark-approved`.
- Help overlay (`?`) gains `D  Mark approved as draft (reverse A)`.
- Website updated: `commands.astro` (CLI table + key table) and
  `faq.astro` ("Email not approved for sending" entry now mentions
  the reverse).
- Tests: 4 new integration tests cover demote-approved, idempotent
  on draft, sent-rejected, and inbox-rejected. 295 unit + all
  integration tests pass.

Note: when this ticket was filed it was tracked in `BACKLOG.md`
under a stale `#0001` line ("hotkey to mark email as draft") --
that slot was later reused for `Auto-fetch on TUI startup`. Both
stale lines are removed from the index in this change.
