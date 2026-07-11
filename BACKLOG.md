# Backlog

Index of open tickets. One file per item lives in [docs/tickets/](docs/tickets/); see [docs/tickets/README.md](docs/tickets/README.md) for the convention. Use the `ticket` fish function to add a new entry.

When a ticket is shipped: set `status: done` in the ticket file, add an entry to [CHANGELOG.md](CHANGELOG.md), and remove its line from this index.

## Now

- [#0022 consistent naming](docs/tickets/0022-consistent-naming.md) -- refactor

## Next

> Calendar invitations (iMIP) design: [docs/plans/calendar-invites.md](docs/plans/calendar-invites.md). v1 is iMIP over SMTP/IMAP for all accounts; no Graph. Supersedes #0020 / #0026.

- [#0027 iMIP receive - detect/save/parse invite](docs/tickets/0027-imip-receive-parse.md) -- feature
- [#0028 iMIP send - REQUEST invite + CLI](docs/tickets/0028-imip-send-invite.md) -- feature
- [#0029 iMIP RSVP - accept/tentative/decline + TUI](docs/tickets/0029-imip-rsvp-reply.md) -- feature
- [#0030 iMIP REPLY reconciliation (organizer-side)](docs/tickets/0030-imip-reply-reconciliation.md) -- feature
- [#0032 TUI foundation package (overlay enum, keymap-as-data, leaders)](docs/tickets/0032-tui-foundation-package.md) -- refactor
- [#0005 Parallel IMAP fetch per mailbox](docs/tickets/0005-parallel-imap-fetch-per-mailbox.md) -- perf
- [#0007 Flagging / starring](docs/tickets/0007-flagging-starring.md) -- feature
- [#0008 Threading / conversation view](docs/tickets/0008-threading-conversation-view.md) -- feature

## Later

> TUI multi-view roadmap: [docs/plans/tui-restructure-views.md](docs/plans/tui-restructure-views.md). Foundation (#0032) first, then views.

- [#0033 View switcher + Contacts view (vCard send)](docs/tickets/0033-view-switcher-contacts-view.md) -- feature
- [#0034 Local calendar view (iMIP-derived)](docs/tickets/0034-local-calendar-view.md) -- feature
- [#0031 iMIP cancellations/updates (CANCEL / SEQUENCE)](docs/tickets/0031-imip-cancel-update.md) -- feature
- [#0035 Graph API admin approval + Azure app verification](docs/tickets/0035-graph-admin-approval.md) -- chore _(blocked)_
- [#0036 Graph sync backend (calendar + server-side RSVP)](docs/tickets/0036-graph-sync-backend.md) -- feature _(blocked by #0035)_
- [#0010 Inline image rendering](docs/tickets/0010-inline-image-rendering.md) -- feature
- [#0016 Open attachments for drafts (`o`)](docs/tickets/0016-attachment-open-for-drafts.md) -- feature
- [#0017 Jump-to-date in mailbox list](docs/tickets/0017-jump-to-date.md) -- feature

### Distribution / cross-platform (adoption track)

> Windows is targeted via WSL only. Native Windows (msvc, Credential Manager, Scoop, winget, EV signing) is out of scope.

- [#0012 Apple Developer ID signing for macOS releases](docs/tickets/0012-apple-developer-id-signing.md) -- chore
- [#0014 Linux packaging (.deb, .rpm, AUR, musl)](docs/tickets/0014-linux-packaging.md) -- chore
- [#0015 Cross-platform smoke tests](docs/tickets/0015-cross-platform-smoke-tests.md) -- chore
