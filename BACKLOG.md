# Backlog

Index of open tickets. One file per item lives in [docs/tickets/](docs/tickets/); see [docs/tickets/README.md](docs/tickets/README.md) for the convention. Use the `ticket` fish function to add a new entry.

When a ticket is shipped: set `status: done` in the ticket file, add an entry to [CHANGELOG.md](CHANGELOG.md), and remove its line from this index.

## Now

- [#0022 consistent naming](docs/tickets/0022-consistent-naming.md) -- refactor

## Next

- [#0005 Parallel IMAP fetch per mailbox](docs/tickets/0005-parallel-imap-fetch-per-mailbox.md) -- perf
- [#0007 Flagging / starring](docs/tickets/0007-flagging-starring.md) -- feature
- [#0008 Threading / conversation view](docs/tickets/0008-threading-conversation-view.md) -- feature
- [#0026 Send calendar invitations (iMIP REQUEST)](docs/tickets/0026-send-calendar-invitations.md) -- feature

## Later

- [#0010 Inline image rendering](docs/tickets/0010-inline-image-rendering.md) -- feature
- [#0016 Open attachments for drafts (`o`)](docs/tickets/0016-attachment-open-for-drafts.md) -- feature
- [#0017 Jump-to-date in mailbox list](docs/tickets/0017-jump-to-date.md) -- feature
- [#0019 Configurable keybindings](docs/tickets/0019-configurable-keybindings.md) -- feature
- [#0020 Accept / tentative / decline calendar invitations](docs/tickets/0020-accept-calendar-invitations.md) -- feature

### Distribution / cross-platform (adoption track)

> Windows is targeted via WSL only. Native Windows (msvc, Credential Manager, Scoop, winget, EV signing) is out of scope.

- [#0012 Apple Developer ID signing for macOS releases](docs/tickets/0012-apple-developer-id-signing.md) -- chore
- [#0013 Homebrew tap](docs/tickets/0013-homebrew-tap.md) -- chore, **blocked**: needs the `homebrew-mailypoppins` tap repo + `TAP_DEPLOY_KEY` deploy-key secret ([manual steps](docs/release-process.md#homebrew-tap)); repo-side work done (deploy-key SSH flow, binary `mp`)
- [#0014 Linux packaging (.deb, .rpm, AUR, musl)](docs/tickets/0014-linux-packaging.md) -- chore
- [#0015 Cross-platform smoke tests](docs/tickets/0015-cross-platform-smoke-tests.md) -- chore
