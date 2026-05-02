# Tickets

One file per unit of work. Plans, handoffs, and research notes link to specific tickets by ID; the [BACKLOG.md](../../BACKLOG.md) at the repo root is a thin index grouped Now / Next / Later.

## Filename convention

```
NNNN-short-slug.md
```

- `NNNN` is a zero-padded 4-digit ID, monotonically increasing. Pick the next number after the highest existing file.
- `short-slug` is lowercase, hyphenated, ideally 2-5 words.

Example: `0007-persist-mailbox-states.md`.

## Frontmatter

```yaml
---
id: 0007
title: Persist mailbox_states across TUI sessions
type: feature        # feature | bug | refactor | perf | idea | chore
priority: now        # now | next | later | icebox
status: open         # open | in-progress | blocked | done | dropped
created: 2026-04-29
---
```

Body is free-form Markdown: problem statement, proposed approach, links to related plans / handoffs / research, acceptance criteria.

## Lifecycle

1. **Create.** Use the `ticket` fish function (`ticket` from any project root) for quick capture, or copy an existing ticket as a template.
2. **Work.** Set `status: in-progress` when you start. Add notes inline as you learn.
3. **Land.** When the change is shipped (`cargo install --path .` and verified):
   - Set `status: done` in the ticket.
   - Add an entry to [CHANGELOG.md](../../CHANGELOG.md) under `[Unreleased]`.
   - Remove the ticket's line from [BACKLOG.md](../../BACKLOG.md).
   - Leave the ticket file in place -- it serves as a record. Move to `docs/tickets/done/` only if the directory grows unwieldy.
4. **Drop.** If a ticket becomes obsolete or duplicated, set `status: dropped` with a one-line reason at the top of the body. Remove from `BACKLOG.md`.

## Linking

- From a plan / handoff / research note: link by relative path: `[persist mailbox_states](../tickets/0007-persist-mailbox-states.md)` or by ID in prose: "see ticket #0007".
- From a ticket: link to plans (`docs/plans/...`), handoffs (`.claude/handoff/...`), research (`.claude/research/...`).

## Why tickets, not a single BACKLOG.md

- One file per item gives plans and handoffs a stable target to link to.
- Less merge churn when several items are touched in one session.
- Per-ticket frontmatter makes filtering trivial (`rg '^priority: now' docs/tickets/`).
- The `BACKLOG.md` index stays glanceable -- one line per item, no descriptions.
