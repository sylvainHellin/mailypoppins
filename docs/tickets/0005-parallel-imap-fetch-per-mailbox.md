---
id: 0005
title: Parallel IMAP fetch per mailbox
type: perf
priority: next
status: open
created: 2026-05-01
---

`sync_mailboxes` uses one IMAP session and SELECTs mailboxes sequentially (IMAP requires one selected mailbox per connection). For accounts with 3+ mailboxes on a remote server, each SELECT+SEARCH+FETCH cycle adds ~200-300 ms of network latency serially.

## Fix

Open N parallel IMAP connections (one per mailbox) so `N * latency` becomes `1 * latency`.

## Trade-off

N TLS handshakes + N logins instead of one. For small N (3-5 mailboxes) the extra handshake cost is < the latency saved.

## Sequencing

Measure post-[#0002 persist-mailbox-states](0002-persist-mailbox-states.md) and [#0003 cold-start](0003-cold-start-async-indexing.md) to confirm this is still the dominant cost.
