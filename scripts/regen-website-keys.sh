#!/usr/bin/env bash
# Regenerate the website TUI key-binding data from the single KEYMAP source of
# truth (src/tui/app/keymap.rs). The website page renders this file, so key
# bindings on mailypoppins.dev cannot drift from the TUI/help overlay.
#
# Run after changing KEYMAP, then rebuild the site (cd website && pnpm build).
set -euo pipefail

cd "$(dirname "$0")/.."

out="website/src/data/tui-keys.json"
cargo run -q -- dump-keys --json > "$out"
echo "Wrote $out ($(grep -c '"key"' "$out") bindings across $(grep -c '"title"' "$out") sections)"
