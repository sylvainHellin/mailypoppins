---
id: 0141
title: Discover every server mailbox, with a per-folder sync policy
type: feature
priority: next
status: open
created: 2026-10-09
---

## Problem

Every mailbox an account uses has to be declared in `config.toml` (`[accounts.mailboxes.inbox|archive|sent]` plus the `[[accounts.mailboxes.extra]]` list).
A folder that is not declared is invisible: `mp sync --mailbox Spesen` is refused with "has no mailbox configured", a default `mp search` never looks there, and `mp search --local` has no rows for it.
The TUM receipts folder `Spesen` was missed this way on 2026-10-09 until it was added by hand.
The first attempt, a named `[accounts.mailboxes.spesen]` table, parsed without a warning and was ignored, because `MailboxesConfig` does not deny unknown keys (PERSO-118).

The live accounts LIST far more than they declare:

- tum (Exchange, `xmail.mwn.de`): 24 folders, 4 configured.
  The rest include `Deleted Items`, `Junk Email`, `Drafts`, `INBOX/reimbursements`, `Archive1`, and non-mail folders (`Calendar`, `Calendar/Birthdays`, `Contacts`, `Tasks`, `Notes`, `Journal`, `RSS Feeds`, `Outbox`, `Conversation History`, `Sync Issues/*`).
- proton (Bridge): 17 folders, 3 configured.
  The rest include `Drafts`, `Trash`, `Spam`, the `Folders` and `Labels` parents, seven `Labels/*` folders, and the virtual `All Mail` and `Starred`.

Syncing everything LIST returns is not the fix.
Every client surveyed (aerc, neomutt, himalaya, mbsync, OfflineIMAP, Thunderbird, Geary) discovers every folder and then filters what it syncs; none hard-codes the list and none syncs all of it by default.

## Pitfalls of naive discovery

Ranked by how badly they would bite, with the code they touch.

1. Sync cost.
   `sync_once` (`src/daemon/runtime/account.rs:924`) syncs every configured target on every tick, 100 UIDs each on a quick tick (`:94`, `:951`) and everything on a full tick (`:952`).
   TUM would go from 4 to 24 SELECT rounds per tick, and a first full sync would pull Calendar, Contacts, Tasks, Notes, Journal and RSS in full.
   Exchange's `MaxConnectionsPerUser` defaults to 16; the parallel fetch in `src/imap_client/store_sync.rs` grows with the target count.
2. Moves into an unsynced folder are pruned.
   The ingest-then-prune ordering of #0072 (`src/sync/engine.rs:1458`) reads a message that left a synced folder for an unsynced one (Junk, Deleted Items) as a server-side removal, so its row disappears and never comes back.
   Every folder that can be a move destination has to be synced.
3. Store-key collisions.
   `MailboxRole::from` (`crates/mp-core/src/types.rs:202`) maps `Inbox`, `Archive` and `Sent` case-insensitively onto the roles, so a discovered folder with one of those names is re-keyed silently.
   The server `Drafts` folder on both accounts collides with the local drafts pseudo-mailbox `DRAFTS_MAILBOX` (`clients/desktop/src-tauri/src/commands.rs:700`, the TUI's `build_mailboxes`).
   `docs/lessons-learned.md:589` records what a key mismatch looks like: a mailbox that stays empty forever.
4. Non-mail and duplicate folders.
   Exchange's Calendar, Contacts, Tasks, Notes, Journal and Sync Issues folders hold non-message items over IMAP.
   Proton's `All Mail` (`\All`) and `Starred` are union views, and every `Labels/*` folder repeats messages that also live in a real folder, so a message would be stored two or three times.
   `\Noselect` parents (`Folders`, `Labels`) fail a SELECT outright.
5. Disk budget.
   `max_disk_bytes` (9.31 GB) is account-wide and the sweep (`src/store/sweep.rs`) evicts oldest-first, so Junk and RSS bodies would push inbox bodies out sooner.
6. Search fan-out.
   The server search (`src/main.rs:4202-4215`) splits `limit` evenly over the targets with a floor of 5 (`:4211`), so at 24 folders INBOX gets 5 hits, and it queries the folders serially.
7. Desktop totals.
   `list_mailboxes_on` (`clients/desktop/src-tauri/src/commands.rs:667-696`) sums total and unread over every row, so the account badge would count Junk and Deleted Items.
8. Names.
   Modified UTF-7 decoding is unverified: `ServerMailbox.name` is the raw SELECT name (`store_sync.rs:~195`), so `Gel&APY-scht` may be stored and shown encoded.
   The `/` delimiter is already in use on TUM; another server may use `.`.
9. Renames and deletions on the server.
   A config-pinned list cannot change under mp; a discovered one can, leaving stranded rows or SELECT errors on every tick.
   Rename handling is the weakest area in every client surveyed (Thunderbird bugs 1994843 and 1997705).
10. Live updates.
    IDLE watches `INBOX` only (`src/daemon/runtime/watcher.rs:60`), so mail in any other folder appears at the next tick (900 s by default).
11. TUI number keys.
    `build_mailboxes` (`clients/tui/src/app/types.rs:2118-2165`) emits Inbox, Drafts, Sent, Archive, then the extras, and `JumpMailbox` (`clients/tui/src/app/keymap.rs:155-157`) binds only `1..=9`.
12. Config parsing.
    No `deny_unknown_fields` on `MailboxesConfig` (`crates/mp-core/src/config.rs:468-478`), so a new key is ignored by an older binary and a typo is ignored by every binary.

## Proposed approach

The design is drawn in [figures/0141-mailbox-catalogue.tex](figures/0141-mailbox-catalogue.tex) (tex-fig; render with `~/code/side-projects/tex-fig/examples/build.sh`).

1. Mailbox catalogue.
   On connect and on every full tick, run `LIST "" "*"`, adding `RETURN (SUBSCRIBED SPECIAL-USE)` when LIST-EXTENDED is advertised, and store the result per account: server name (decoded for display), delimiter, attributes, role, policy.
   `list_mailboxes_detailed` (`store_sync.rs:208-233`) already returns name, delimiter and attributes; nothing consumes the attributes yet, and `mp list-mailboxes` already shows the raw list.
   Graph accounts fill the same catalogue from `list_folders()` and its well-known folder names (`src/graph.rs:505`).
2. Roles.
   Resolve each role in this order: config override, then the SPECIAL-USE attribute, then a provider profile (Exchange, Proton Bridge), then a name heuristic (`Sent Items`, `Sent`, `Archive`, `Deleted Items`, `Junk Email`, `Spam`, `Trash`).
   Add `drafts`, `trash` and `junk` roles beside `inbox`, `archive` and `sent`.
   Today `attribute_token` (`store_sync.rs:~243`) passes `\Sent` and friends through as `Extension`, and LIST is issued without `RETURN (SPECIAL-USE)`.
3. Per-folder policy: `sync`, `status-only` or `ignore`.
   - `sync`: the role folders, plus anything pinned in config; synced every tick as today.
   - `status-only`: counts from STATUS, full fetch when the folder is opened or on a full tick with a small limit.
     Default for user folders, `Deleted Items`, `Junk Email`, and `Labels/*`.
   - `ignore`: never selected.
     Default for `\Noselect` and `\NonExistent`, Exchange's non-mail folders and `Sync Issues/*`, Proton's `All Mail` and `Starred`.
   Defaults come from the role and a built-in exclude list; config holds only overrides (globs plus a policy).
   A folder that is a move destination is never `ignore` for pruning purposes (pitfall 2).
4. Keys.
   Key discovered folders by their server name as `MailboxRole::Other`, refuse a name that collides with a role or `DRAFTS_MAILBOX`, and record UIDVALIDITY so a vanished folder is marked `missing` rather than deleted, with rename matched by UIDVALIDITY plus Message-ID overlap.
5. Clients.
   Number keys `1..=9` bind to roles and pinned folders; every catalogue entry is reachable from a fuzzy folder picker in the TUI and the desktop, as aerc and neomutt do.
   The desktop badge counts `sync` folders only.
6. Search and sync targets.
   `resolve_targets` and `find_sync_target` (`src/daemon/methods/sync.rs:549-575`) accept any catalogue name, so `--mailbox Spesen` works without a config edit.
   A default search reads the local index first and queries the server for the role set and the current folder, with a weighted quota instead of `limit / targets`.
7. Config.
   Keep `[accounts.mailboxes.*]` working unchanged as role overrides and pins, and add `deny_unknown_fields` or a warning for unknown keys (PERSO-118).

## To verify before building

- Run `CAPABILITY` and `LIST "" "*" RETURN (SPECIAL-USE)` against `xmail.mwn.de` and Proton Bridge: whether Exchange advertises SPECIAL-USE, LIST-EXTENDED, LIST-STATUS and CONDSTORE, and which folders carry `\Noselect`.
- Whether the store deduplicates Proton label copies by Message-ID or keeps one row per folder.
- How the sweep, the archive mutation and the sent copy resolve their folders (`find_server_name_for_role` callers were not traced).

## Acceptance

- A folder that exists on the server is reachable by `mp sync --mailbox`, `mp search --mailbox` and the folder picker without a config edit.
- A quick tick on TUM selects only the `sync` folders.
- No Proton message is stored twice because of a label or `All Mail`.
- A message moved into Junk or Deleted Items keeps a row.
- A server folder named `Drafts`, `Sent` or `Archive` never shadows a role or the local drafts.
- Existing configs keep working unchanged.

## Research

- [IMAP mailbox auto-discovery](../../.agents/research/2026-10-09-imap-mailbox-auto-discovery.md): protocol, Exchange and Bridge behaviour, client survey with sources (local, gitignored).
- [Code map](../../.agents/research/2026-10-09-mailbox-discovery-code-map.md): every consumer of the configured-mailbox list (local, gitignored).
- Plane PERSO-119.
