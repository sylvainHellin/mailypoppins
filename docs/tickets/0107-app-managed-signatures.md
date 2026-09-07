---
id: 0107
title: App-managed signature files with a management overlay
type: feature
priority: later
status: open
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
