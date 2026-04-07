# Backlog

## Now

Items to tackle in the current work cycle.

- **Fix read/unread sync behaviour** -- read status resets after each fetch. Bidirectional sync with IMAP `\Seen` flag is not working correctly. [handoff](.claude/handoff/2026-04-05_23-34-46_read-unread-sync-debug.md)

## Next

Queued up, will pick from here when Now is clear.
- **Flagging / starring** -- flag important emails on the server, display flag icon in the list, filter flagged.
- **Threading / conversation view** -- group emails by `In-Reply-To` / `References` headers. Show conversation as an expandable tree or inline thread. [plan](docs/plans/threading.md)
- **Desktop notifications** -- IDLE watcher detects new mail but gives no notification outside the TUI. Integrate `notify-rust` or macOS `terminal-notifier`.

## Later

Ideas and longer-term improvements. Not committed to yet.
- **Contact autocomplete** -- address book from sent/received history, autocomplete when editing draft frontmatter.
- **Inline image rendering** -- render inline images in terminal preview (sixel, iTerm2 protocol, or Kitty graphics).
- **CI / release pipeline** -- GitHub Actions for build, test, and binary release on tag push.
