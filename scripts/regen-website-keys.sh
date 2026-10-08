#!/usr/bin/env bash
# Regenerate the website's key bindings page from the single KEYMAP source of
# truth (clients/tui/src/app/keymap.rs, through `mp dump-keys --json`) and the
# desktop keymap (clients/desktop/src/keymap/catalog.ts), with the mp built
# from this checkout. The page cannot drift from the TUI help overlay.
#
# Run after changing KEYMAP or catalog.ts, then rebuild the site
# (cd website && pnpm build). `pnpm gen:keys` in website/ does the same with
# the mp on PATH.
set -euo pipefail

cd "$(dirname "$0")/.."

cargo build -q --bin mp
target_dir=$(cargo metadata --format-version 1 --no-deps | python3 -c 'import sys,json;print(json.load(sys.stdin)["target_directory"])')
MP_BIN="$target_dir/debug/mp" node website/scripts/gen-keys.ts
