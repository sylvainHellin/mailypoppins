#!/usr/bin/env bash
# Capture the whole `mp --help` surface as one document, by the same algorithm
# as tests/cli_help_snapshot.rs: run the real binary, print a `$ mp … --help`
# header before each screen, then recurse into every name listed under
# `Commands:` (clap's auto-generated `help` excluded).
#
# The test snapshots this walk through insta; this script writes it to stdout so
# a baseline artifact can be captured from an installed binary without going
# through cargo. The two must agree - see
# docs/baselines/pre-daemon/README.md.
#
# Usage: scripts/capture-cli-help.sh [> out.txt]
#        MP=./target/release/mp scripts/capture-cli-help.sh
set -euo pipefail

MP="${MP:-mp}"

# Run `mp <args> --help` and echo stdout. Fails loudly if the call errors or
# writes to stderr, so a broken subcommand cannot slip in as an empty section.
help_for() {
  local err out status
  err="$(mktemp)"
  set +e
  out="$("$MP" "$@" --help 2>"$err")"
  status=$?
  set -e
  if [ "$status" -ne 0 ]; then
    echo "mp $* --help failed with status $status" >&2
    cat "$err" >&2
    rm -f "$err"
    exit 1
  fi
  if [ -s "$err" ]; then
    echo "mp $* --help wrote to stderr:" >&2
    cat "$err" >&2
    rm -f "$err"
    exit 1
  fi
  rm -f "$err"
  printf '%s\n' "$out"
}

# Subcommand names listed in the `Commands:` block, in the order clap prints
# them. Continuation lines of a wrapped description are indented past the name
# column, so a two-space indent (and no more) identifies a name.
subcommand_names() {
  awk '
    $0 == "Commands:" { in_cmds = 1; next }
    !in_cmds { next }
    /^[[:space:]]*$/ { exit }
    /^   / { next }
    /^  / { if ($1 != "help") print $1 }
  '
}

collect() {
  local help name
  help="$(help_for "$@")"
  if [ "$#" -eq 0 ]; then
    printf '$ mp --help\n'
  else
    printf '$ mp %s --help\n' "$*"
  fi
  printf '%s\n\n' "$help"

  while read -r name; do
    [ -n "$name" ] || continue
    collect "$@" "$name"
  done < <(printf '%s\n' "$help" | subcommand_names)
}

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
collect >"$tmp"

# Sanity: the walk really recursed rather than capturing one screen.
screens="$(grep -c '^\$ mp' "$tmp")"
if [ "$screens" -le 20 ]; then
  echo "the help walk collected $screens screens" >&2
  exit 1
fi

cat "$tmp"
