# Mailbox discovery (#0141)

Implementation plan and map for [ticket #0141](../tickets/0141-mailbox-auto-discovery.md), which holds the design and Sylvain's policy decision of 2026-10-22.
The catalogue and the folder roles come first, because the server drafts of #0143 reuse the `drafts` role.

## Layers

- `src/catalogue/discover.rs` asks the server: IMAP `LIST "" "*"` through `list_mailboxes_detailed`, one `STATUS (MESSAGES UNSEEN)` per `status-only` folder through `imap_client::status_counts`, and Graph's folder tree (`GraphClient::list_folder_tree`) with the six well-known folders' ids as role attributes.
- `src/catalogue/listing.rs` keeps the answer in the store's additive `mailbox_listing` table: raw facts, a `missing` mark for a vanished folder, and the server counts.
- `src/catalogue/mod.rs` resolves the stored listing against the live config into the `Catalogue`: role, policy, store key, label, reason.
  It is pure, and every consumer reads it: `targets()`, `sidebar()`, `sync_target()`, `storage_for()`, `server_for_key()`, `find()`.
- `src/catalogue/utf7.rs` decodes modified UTF-7 for the label only; the key and the `SELECT` name stay as the server spells them.

## Flow

1. The runtime's body hook lists the server on its first tick and on every full tick (`refresh_catalogue` in `src/daemon/runtime/account.rs`); a whole-account `mp sync` and an account's first sync list too (`body` in `src/daemon/methods/sync.rs`).
2. The tick's targets are `Catalogue::targets()`: the pins in config order, the trash and junk tails (`SyncTarget::window`), then every other `sync` folder.
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
- `mp list-mailboxes` prints each folder's policy and role or reason (`read_cmd::server_mailbox_lines`).
- The TUI sidebar scrolls to keep the open or highlighted mailbox on screen when an account has more mailboxes than the pane has rows.

## Choices made in the implementation

- The server drafts folder is catalogued with the `drafts` role and ignored until #0143 adds its reconcile and its own key, so the sidebar does not list two Drafts.
- `status-only` means server counts for every such folder, plus the newest 50 messages for the trash and junk folders, which is what keeps a moved message's row (pitfall 2); a label folder downloads nothing.
- No fetch-on-open of a `status-only` folder: an explicit `mp sync --mailbox <folder>` downloads one in full.
- The provider exclude lists apply to the provider the listing looks like, never to every account.
- No `RETURN (SPECIAL-USE)`: both live servers mark the role folders in a plain `LIST`.

## Not built

- Rename detection by UIDVALIDITY and Message-ID overlap (pitfall 9): a renamed folder syncs from scratch, the old one stays `missing` with its rows.
- IDLE stays on INBOX (pitfall 10); a discovered folder's mail arrives at the next tick.
