---
id: 0107
title: App-managed signature files with a management overlay
type: feature
priority: later
status: done
created: 2026-08-16
---

## Problem

Signatures live in `config.toml` as `[accounts.signatures.*]` tables, either inline `text` or a `path` to a file.
That splits one concept across two representations and forces the user to hand-edit TOML to add, rename, or remove a signature.
#0106 added TUI selection and `$EDITOR` editing, but creation and renaming still require editing the config file by hand, and inline `text` cannot be edited persistently at all.

Signature content is content, not connection config, so it belongs in the app-managed layer alongside drafts, not in the user-owned `config.toml`.

Owner decision, 2026-08-16: move signatures fully into the app.
Content goes to app-managed `.md` files; the per-account default selection goes to an app state file, so `config.toml` carries no signature keys at all.

## Approach

### Storage
Each signature is one Markdown file in `~/.config/mailypoppins/signatures/<name>.md`.
The file name (without extension) is the signature's key; the display name is either the key or a first-line convention, to be decided during the build.
Drop inline `text` entirely: a signature has exactly one representation, a file.

### Default selection
The per-account default signature moves out of `config.toml` into an app state file (the same layer the app already uses for its own state).
`config.toml` keeps no signature keys once migration has run.

### Migration
On first run after this ships, read any existing `[accounts.signatures.*]` tables: write each `text` or `path` entry out to `signatures/<name>.md`, record the old `default` in the app state file, then leave a one-line notice pointing the user to remove the now-dead tables (do not rewrite the user's `config.toml`, matching the #0022 precedent of warning rather than editing it).

### Management overlay
A dedicated overlay, reachable from `?` (so it is discoverable, unlike the wizard-internal keys of #0106):
- list the account's signatures with the default marked,
- pick the default,
- `e` to edit the selected file in `$EDITOR`,
- `n` to create a new signature (prompt for a name, open `$EDITOR` on the new file),
- rename and delete.
This is a new `Overlay` variant, a keymap entry, a renderer in `src/tui/ui/overlays.rs`, and file-level CRUD in a signatures module.

### Wire-through
`resolve_signature_markdown` and the compose wizard's `available_signatures` read from the signatures directory and the state file instead of `AccountConfig::signatures`.
The #0106 sentinel splicing and hard-break normalisation are unchanged.

## Acceptance

Signatures can be created, renamed, edited, deleted, and set as default entirely from the TUI.
`config.toml` holds no signature keys after migration.
An existing `config.toml` with `[accounts.signatures.*]` migrates cleanly to files plus the state file, with the old default preserved.
The signatures overlay is listed in `?`.

## Links

Extends [#0106](0106-markdown-native-signatures-tui-edit.md) (Markdown-native signatures, hard breaks, wizard selection and editing).

## Done (2026-09-07)

Both parts shipped, in `68c1700` (storage, migration, wire-through) and `69f3a1d` (the overlay).

Storage is `src/signatures.rs`: one Markdown file per signature at `config_dir()/signatures/<name>.md`, the file stem being both the key and the display name, so the directory listing is the whole index and there is nothing to keep in sync.
The open question in Approach (key versus a first-line convention) was settled on the stem.
Inline `text` is gone; `SignaturesConfig` and `SignatureEntry` survive in `config.rs` only so an untouched `config.toml` still parses and the migration can read them, and the field doc says nothing else may.
`validate_name` rejects rather than sanitises: a silently rewritten name means the user asks for `../evil`, gets `evil`, and then cannot find the file they made.

The default selection went to `src/app_state.rs`, a pretty-printed `state.json` under the data dir modelled on `contacts::cache`.
A missing file, a missing account, a missing key and a corrupt file all mean "nothing recorded"; losing a preference must not stop the app from starting.
`default_signature_name` also checks that the file still exists, so a default pointing at a deleted signature is no default.

`migrate_config_signatures` runs from `main()` before anything resolves a signature, writes each legacy entry (inline `text` verbatim, else the `path` file's bytes) out to its file, records the old default when the account has none yet, and warns that the tables are dead.
It never rewrites `config.toml` (the #0022 precedent), is idempotent, skips a name whose file already exists so a signature edited since is never clobbered, and is non-fatal per entry: an unreadable `path` warns and skips rather than failing startup.
The notice repeats on every run while the dead tables are still in the file, which is the only nudge the user gets.

`resolve_signature_markdown` (`config.rs`) now reads the signatures directory and the state file; a missing file logs rather than printing to stderr, since by then the TUI owns the terminal.
The compose wizard lists from the directory and `e` always edits the file in place, which retired the inline temp-file branch and the `signature_override` plumbing of #0106.
`mp config init` no longer carries signature tables across a rewrite, and `mp config show` prints the directory, the resolved default, and a one-line preview per file plus a warning when legacy tables are still present.

Part 2 is `Overlay::Signatures`, bound to `cs` in the `Global` context so it appears in `?` and the command palette.
It lists the account's signatures with the default starred: `Enter` sets or clears the default, `e` edits in `$EDITOR`, `n` prompts for a name and creates, `r` renames, `d` deletes behind the existing confirm dialog.
Prompt state is a `SignaturesMode` enum, following the `DirPicker` precedent.
Every mutation goes through `src/signatures.rs`, so its validation errors reach the user instead of failing silently, and the cached account signature is re-resolved after each change.
A golden frame covers the overlay; the help and command-palette frames shifted because both derive from `KEYMAP`.
