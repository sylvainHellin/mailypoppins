# Mailbox discovery (#0141)

Implementation plan and map for [ticket #0141](../tickets/0141-mailbox-auto-discovery.md), which holds the design and Sylvain's policy decision of 2026-10-22.
The catalogue and the folder roles come first, because the server drafts of #0143 reuse the `drafts` role.

## Layers

- `src/catalogue/discover.rs` asks the server: IMAP `LIST "" "*"` through `list_mailboxes_detailed`, one `STATUS (MESSAGES UNSEEN)` per `status-only` folder through `imap_client::status_counts`, and Graph's folder tree (`GraphClient::list_folder_tree`) with the six well-known folders' ids as role attributes.
- `src/catalogue/listing.rs` keeps the answer in the store's additive `mailbox_listing` table: raw facts, a `missing` mark for a vanished folder, the server counts, and on Graph the folder id (`folder_id`, added to a table an earlier build created by `listing::ensure_columns`).
- `src/catalogue/mod.rs` resolves the stored listing against the live config into the `Catalogue`: role, policy, store key, label, reason.
  It is pure, and every consumer reads it: `targets()`, `sidebar()`, `sync_target()`, `storage_for()`, `server_for_key()`, `find()`.
- `src/catalogue/utf7.rs` decodes modified UTF-7 for the label of an IMAP folder only; the key and the `SELECT` name stay as the server spells them, and a Graph display name is never decoded.
  A run that yields no whole character or leaves set bits over is shown as sent, so `Q&A-2024` stays `Q&A-2024`.

## Flow

1. The runtime's body hook lists the server on its first tick and on every full tick (`refresh_catalogue` in `src/daemon/runtime/account.rs`); a whole-account `mp sync` and an account's first sync list too (`body` in `src/daemon/methods/sync.rs`).
2. The tick's targets are `Catalogue::targets()`: the pins in config order, the trash and junk tails (`SyncTarget::window`), then every other `sync` folder.
   A windowed target takes the newest `window` messages of the folder, held or not, on both transports (IMAP's newest UIDs, Graph's newest by `receivedDateTime`), and never reports a backlog, so back to back passes do not drain the folder.
3. After the pass, `refresh_status_counts` records the server counts of the `status-only` folders.
4. `publish_changed_counts` re-seeds the account in the canonical state (`CanonicalState::reseed_account`), announces every new or changed row with a counts invalidation and every dropped row with a `state.remove`, then publishes the effective counts (the server's for a `status-only` row).
5. The TUI's recount adopts the hierarchy the `mailbox.list` answer carries (`App::adopt_mailbox_rows`); the desktop re-reads `mailbox.list` on the invalidation.

## Consumers moved onto the catalogue

- `mailbox.list`, the bootstrap seeds (`seed_of`), `message.list`, `message.search`, move destinations (`resolve_mailbox` / `resolve_in`).
- `sync.quick` / `sync.full` with `mailbox` (`resolve_targets`), hooks (`hooks::mailbox_key`), `hook.list`.
- `message.search_server`: default targets are the `sync` rows, any listed folder by name.
- `message.fetch`: `fetch_storage`, any listed folder whatever its policy, refusing an unlisted or shadowing name.
- The move, delete and flag paths (`server_for_key`), the Sent copy's folder (`DurableSend::begin`), `mp dump-mailbox`.
- The CLI resolves `--mailbox` names through the daemon's `mailbox.list` (`daemon_mailboxes`, `select_mailboxes` in `src/main.rs`), no longer from its own config.
- A name a person typed matches a store key, then a sidebar label, then a server name, one tier at a time across every row (`mp_protocol::state::pick_mailbox`), in the daemon, `mp`, the TUI and the search targets: a pinned extra called `Trash` is that folder, not the `\Trash` folder the sidebar labels `Trash`.
- On Graph a config name Graph resolves to a well-known folder (`Inbox`, `Sent Items`, `Deleted Items`) designates the listed folder carrying that role, so an English config on a German mailbox does not sync `Posteingang` twice (`configured_folder`).
- A selector `mp show` or `mp save` cannot find is handed to `message.fetch` whatever its mailbox; the daemon refuses an unlisted name in its own words.
- On Graph, a search across every folder labels a hit's `parentFolderId` from the listing's ids with no request, an ignored child folder included under its server name, and a named search or a fetch addresses the folder by id.
- `mp config show` prints `[folders]`, every folder of the last listing with its policy, from `mailbox.list` with `ignored: true`.
- The setup wizards (`mp config init`, `mp config add-account`, the desktop account wizard) ask for the drafts, trash and junk folders, each empty for the one the server marks, and for policy overrides, and no longer pin extra folders.
- `mp list-mailboxes` prints each folder's policy and role or reason (`read_cmd::server_mailbox_lines`).
- The TUI sidebar scrolls to keep the open or highlighted mailbox on screen when an account has more mailboxes than the pane has rows.

## Choices made in the implementation

- The server drafts folder is catalogued with the `drafts` role and ignored until #0143 adds its reconcile and its own key, so the sidebar does not list two Drafts.
- `status-only` means server counts for every such folder, plus the newest 50 messages for the trash and junk folders, which is what keeps a moved message's row (pitfall 2); a label folder downloads nothing.
- No fetch-on-open of a `status-only` folder: an explicit `mp sync --mailbox <folder>` downloads one in full.
- The provider exclude lists apply to the provider the listing looks like, never to every account.
- No `RETURN (SPECIAL-USE)`: both live servers mark the role folders in a plain `LIST`.

## Folder renames

Graph renames are detected and followed (`listing::replace`, `discover::follow_rename`).
A folder listed under a new name with the `folder_id` of a folder the listing lost is the same folder: its listing row takes the new name with its counts, and its messages, sync cursor (with the delta token, still valid since the id did not change), mailbox record and ingest failures move from the old store key to the new one.
A message the new key already holds stays there and the old copy is deleted.
Nothing moves when the key does not change (a role folder), when config pins the old name (the log says to edit the config), or when the new name would shadow a role or the local Drafts.

IMAP renames are not detected yet; a renamed folder syncs from scratch and the old one stays `missing` with its rows.
IMAP has no folder id, so the plan for it is:

1. In `discover::record`, after `listing::replace`, pair each folder that appeared with each folder that went missing in the same listing; no pair, nothing to do, which keeps the common tick free of extra I/O.
2. For each pair, one `STATUS <new> (UIDVALIDITY MESSAGES)` and the old key's stored `uidvalidity` from `sync_cursors`; a server that keeps UIDVALIDITY across a `RENAME` makes a match the strong signal.
   Which of the live servers (TUM Exchange, Proton Bridge) keep it is to be checked with a rename on each before this is built.
3. On a UIDVALIDITY match, confirm with message ids: `UID FETCH 1:* (BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])` on the newest 20 UIDs of the new folder against the old key's rows; at least half present (or all, when the folder holds fewer than 4) is a rename.
   A server that issues a new UIDVALIDITY on rename falls back to the ids alone with a stricter bar: at least 10 of the newest 20, so two folders holding a copied handful do not merge.
4. A confirmed rename goes through the same `follow_rename` the Graph path uses, with the listing row renamed in `listing::replace`'s transaction rather than marked missing.
   The IMAP cursor is kept only on a UIDVALIDITY match; on an id-only match the cursor is dropped and the next pass relists, since the old UIDs mean nothing in the new folder, and the moved rows are re-keyed to the new UIDs by the sync's existing `uid_rebound` path.
5. Tests: a fake session answering `STATUS` and the header fetch for a rename with kept UIDVALIDITY, a Bridge-style rename with a new one, a copy that must not merge, and a pinned old name that must not move.

## Not built

- IMAP rename detection, planned above.
- IDLE stays on INBOX (pitfall 10); a discovered folder's mail arrives at the next tick.
