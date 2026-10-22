---
id: 0141
title: Discover every server mailbox, with a per-folder sync policy
type: feature
priority: next
status: done
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

## Design

The design is drawn in [figures/0141-mailbox-catalogue.tex](figures/0141-mailbox-catalogue.tex) (tex-fig; render with `~/code/side-projects/tex-fig/examples/build.sh`); the code is `src/catalogue/`, and `docs/plans/mailbox-discovery.md` maps it.

1. Mailbox catalogue.
   The account runtime's first tick and every full tick run `LIST "" "*"` and store the answer in the store's additive `mailbox_listing` table (`src/catalogue/listing.rs`): name, delimiter, attributes, counts, and a `missing` mark for a folder the server stopped listing.
   A whole-account `mp sync` and an account's first sync list too.
   The plain `LIST` is enough: neither live server advertises SPECIAL-USE and both mark the role folders in a plain listing, so `RETURN (SPECIAL-USE)` is not sent.
   `attribute_token` now spells the RFC 6154 attributes `\Sent`, `\Drafts`, `\Trash`, `\Junk`, `\Archive`, `\All` and `\Flagged`, where it used to fall through to a `Debug` rendering without the backslash.
   Graph fills the same table from the whole folder tree (`GraphClient::list_folder_tree`, children as `Parent/Child`) and gives each well-known folder the attribute of its role.
   Role and policy are not stored: `Catalogue::resolve` derives them from the stored listing and the live config on every read, so a config edit takes effect without a new `LIST`.
2. Roles.
   `inbox`, `archive`, `sent`, `drafts`, `trash` and `junk`, each resolved in this order: the config mapping (`[accounts.mailboxes.<role>]`), the attribute, then the names Exchange, Proton Bridge, Dovecot and Gmail give the folder (also under an `INBOX` parent).
   A pinned `extra` never takes one of the three keyed roles, so its rows keep their key.
3. Policy, per Sylvain's decision of 2026-10-22.
   - `sync`: the role folders, every user folder and every pin; synced every tick like Inbox.
   - `status-only`: the trash and junk folders and Proton's `Labels/*`; the sidebar shows the server's counts, read by one `STATUS (MESSAGES UNSEEN)` per folder after every tick.
     The trash and junk folders also download their newest 50 messages every tick (`STATUS_ONLY_WINDOW`), so a message another client moved there lands in the same pass that prunes its source row (pitfall 2); such a capped target's short download does not hold the account's prune gate shut (`SyncTarget::window`).
     The tail is the folder's newest 50 whether the store holds them or not, on Graph as on IMAP, and it never owes a backlog, so back to back passes do not drain the folder.
     A label folder downloads nothing, so no Proton message is stored twice.
   - `ignore`: `\Noselect` and `\NonExistent`, `\All` and `\Flagged` views, Exchange's non-mail folders and their subtrees (`Calendar`, `Contacts`, `Tasks`, `Notes`, `Journal`, `RSS Feeds`, `RSS Subscriptions`, `Outbox`, `Conversation History`, `Sync Issues`), Proton's `All Mail` and `Starred`, the server drafts folder until #0143 mirrors it, and a folder whose name would shadow a role.
   The exclude lists apply to the provider the listing looks like (Exchange by two of its non-mail folders, Bridge by its `Folders` and `Labels` parents), so a Dovecot folder called `Notes` syncs.
   `[[accounts.mailboxes.override]]` entries (`match` glob, `policy`) change any of that except a pin, an unselectable folder and a shadowing name.
4. Keys and names.
   `inbox`, `archive` and `sent` keep their role keys; every other folder is keyed by its server name, which is what an `extra` entry always keyed it by.
   A name a person types matches a store key, then a sidebar label, then a server name, one tier at a time across every row, in every client (`mp_protocol::state::pick_mailbox`).
   On Graph a config name Graph resolves to a well-known folder (`Inbox`, `Sent Items`) is the localized folder the listing marks with that role, and a Graph display name is never decoded as modified UTF-7.
   An IMAP name is decoded for its label only, and a run that is not modified UTF-7 (`Q&A-2024`) is shown as sent.
   A folder whose name reads back as a role key or as the local drafts view (`Sent` beside the `\Sent` folder, the server `Drafts`) is ignored and refused by every resolver.
   A folder the server stops listing is marked `missing`: its rows stay, it is neither synced nor shown, and it comes back when the server lists it again.
5. Clients.
   `mailbox.list` and the bootstrap seeds come from `catalogue::sidebar`: Inbox, Drafts, Sent, Archive, then trash, junk, the pins, the other synced folders and the counts-only folders, each row with its `server` and `policy`.
   The number keys reach the first nine; `gf` opens a fuzzy picker over every mailbox in the TUI and the desktop.
   A tick that changes the hierarchy re-seeds the account and announces the new rows, which both clients re-read; the desktop's account totals count `sync` rows only.
6. Search, sync, fetch and move.
   `mp sync --mailbox`, a hook's `mailbox`, `message.list`, `message.search`, a move destination and `message.fetch` all resolve through the catalogue, so any folder the server lists is reachable without a config edit.
   A server search that names no mailbox runs over the `sync` folders; a named folder of any policy is searched.
   A server-search hit from any listed folder is fetchable and stored under that folder's key, a counts-only or ignored folder included, by `mp show` and `mp save` as by the TUI and the desktop.
   On Graph the listing stores each folder's id, so a search across every folder labels its hits with no request, an ignored child folder included, and a named search or a fetch addresses the folder by id.
   The move, delete and flag paths and the Sent copy read the server name behind a key from the catalogue.
7. Config.
   `[accounts.mailboxes.inbox|archive|sent]` and the `extra` list keep working unchanged as pins; `drafts`, `trash` and `junk` name a role folder the server does not mark; unknown keys warn (PERSO-118).
   The setup wizards (`mp config init`, `mp config add-account`, the desktop account wizard) ask for the drafts, trash and junk folders and for overrides, and pin no extra folders.
   `mp config show` lists every folder of the last listing with its policy, ignored ones included.
8. Renames (pitfall 9).
   A Graph folder that comes back under a new name with the same folder id takes its rows, sync cursor and delta token to its new key; a pinned old name is left alone and logged.
   IMAP renames are not detected yet: the renamed folder syncs from scratch and the old one stays `missing` with its rows; the detection by UIDVALIDITY and Message-ID overlap is planned in `docs/plans/mailbox-discovery.md`.

## Decisions

- A newly found user folder syncs every tick like Inbox; the trash and junk folders and Proton's label folders get counts only; Exchange's calendar and contact folders and Proton's `All Mail` and `Starred` are ignored (policy table above).
- The catalogue and the folder roles land first, then the server drafts of [#0143](0143-server-drafts.md), which reuse the `drafts` role.
- The implementation choices (the 50-message tail, the hidden server Drafts, no full fetch on opening a counts-only folder, colliding names ignored, the override syntax, the name matching order, Graph's English pins, the wizards, renames) are in `decision-log.md` under 2026-10-22.

## Verified before building

- Answered by the draft probe of 2026-10-20 ([#0143](0143-server-drafts.md), "Probe results"): neither server advertises `SPECIAL-USE`, `CONDSTORE` or `QRESYNC`, and both offer `UIDPLUS`, `MOVE` and `IDLE`.
  Both mark `\Drafts`, `\Sent`, `\Trash` and `\Junk` in the plain LIST, which is all the role detection needs; Proton also marks `\Archive`, `\All` and `\Flagged`, and TUM's `Archive` carries no mark, so it stays a config entry.
  Proton's `Folders` and `Labels` parents carry `\Noselect`; TUM has none.
- The store keeps one row per `(mailbox, uid)` and does not deduplicate Proton label copies by Message-ID, which is why a label folder downloads nothing.
- The move, the delete, the flag pass and the Sent copy resolved a store key to a server name through `find_server_name_for_role`, which falls back to the key itself; they now read the catalogue, which also knows a role found on the server.

## Acceptance

- A folder that exists on the server is reachable by `mp sync --mailbox`, `mp search --mailbox` and the folder picker without a config edit (`mp_sync_mailbox_reaches_any_listed_folder`, `the_folder_picker_opens_any_mailbox_by_name`, `folderPicker.test.tsx`).
- A quick tick on TUM selects only the `sync` folders, plus the 50-message tail of Deleted Items and Junk Email (`tum_syncs_its_role_and_user_folders_and_tails_trash_and_junk`).
- No Proton message is stored twice because of a label or `All Mail` (`proton_never_downloads_a_label_all_mail_or_starred`).
- A message moved into Junk or Deleted Items keeps a row (`a_windowed_targets_short_download_does_not_defer_the_prune`), as long as it is among the folder's newest 50; on Graph the tail stays the newest 50 pass after pass (`a_windowed_target_takes_the_newest_window_and_owes_no_backlog`).
- A pinned extra called `Trash` is that folder by name, not the `\Trash` folder (`an_extra_named_like_a_role_label_resolves_to_its_own_key`).
- An English config on a German Graph mailbox syncs each role folder once (`a_graph_pin_by_its_english_name_is_the_localized_role_folder`).
- `mp show` reads a hit from an ignored folder (`a_hit_from_an_ignored_folder_is_fetched_by_mp_show`).
- A renamed Graph folder keeps its rows and cursor (`a_renamed_graph_folder_takes_its_rows_and_cursor_along`).
- A server folder named `Drafts`, `Sent` or `Archive` never shadows a role or the local drafts (`a_folder_named_like_a_role_never_shadows_it`).
- Existing configs keep working unchanged (`an_existing_config_keeps_its_targets_and_its_keys`).

## Research

- [IMAP mailbox auto-discovery](../../.agents/research/2026-10-09-imap-mailbox-auto-discovery.md): protocol, Exchange and Bridge behaviour, client survey with sources (local, gitignored).
- [Code map](../../.agents/research/2026-10-09-mailbox-discovery-code-map.md): every consumer of the configured-mailbox list (local, gitignored).
- Plane PERSO-119.
