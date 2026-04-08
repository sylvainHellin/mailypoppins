#!/usr/bin/env bash
# Build, install, and (on macOS) re-sign the email binary.
# This is the canonical way to install for local development.
#
# On Linux / Windows, the codesign step is a no-op and this is equivalent to
# `cargo install --path .` directly.
#
# Forwards extra args to cargo, e.g. `./scripts/install.sh --locked`.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo install --path . "$@"
exec ./scripts/codesign-macos.sh
