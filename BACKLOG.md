# Backlog

## Now

Items to tackle in the current work cycle.

- ~~**Unit & integration tests**~~ -- DONE. 139 tests across 11 modules. See [plan](docs/plans/tests.md).
- ~~**Refactor large TUI modules**~~ -- DONE. Split `app.rs`, `ui.rs`, `mod.rs`, `imap_client.rs` into focused submodules. No file exceeds 824 lines. [plan](docs/plans/tui-refactor.md)

## Next

Queued up, will pick from here when Now is clear.

- **Mark as read / unread** -- toggle read status on the server from the TUI. Currently all fetched emails are implicitly marked read.
- **Flagging / starring** -- flag important emails on the server, display flag icon in the list, filter flagged.
- **Threading / conversation view** -- group emails by `In-Reply-To` / `References` headers. Show conversation as an expandable tree or inline thread. [plan](docs/plans/threading.md)
- **Desktop notifications** -- IDLE watcher detects new mail but gives no notification outside the TUI. Integrate `notify-rust` or macOS `terminal-notifier`.

## Later

Ideas and longer-term improvements. Not committed to yet.
- **Contact autocomplete** -- address book from sent/received history, autocomplete when editing draft frontmatter.
- **Inline image rendering** -- render inline images in terminal preview (sixel, iTerm2 protocol, or Kitty graphics).
- **CI / release pipeline** -- GitHub Actions for build, test, and binary release on tag push.
