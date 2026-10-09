#!/usr/bin/env bash
# Pull main and install the CLI, the TUI and the desktop app from source on a
# Mac (PERSO-91).
#
# Usage: scripts/install-local.sh [--no-pull] [--no-gui] [--allow-dirty] [--dry-run] [-h]
#
# What it does, in order:
#   1. repo: refuse a tree with tracked changes, `git pull --rebase --autostash`.
#   2. cli:  `cargo install --path . --locked`, the one `mp` that is the CLI and
#            the TUI, into ~/.cargo/bin (or $CARGO_INSTALL_ROOT / $CARGO_HOME).
#   3. gui:  `pnpm install` when the lockfile moved, `pnpm bundle` with that same
#            `mp` as the sidecar, quit the running app, swap the new bundle into
#            /Applications/mailypoppins.app.
#   4. daemon: restart it once on the new binary, then relaunch the app if it
#            was running.
#   5. summary: commit, versions, daemon state.
#
# Decisions, with the line that settles each:
#
# - The daemon is not stopped before `cargo install`. docs/release-process.md,
#   "After a `cargo install`, restart the daemon": the install "replaces the
#   binary on disk and leaves the running daemon exactly where it was"; macOS
#   lets a running executable be replaced, so the restart afterwards is the
#   whole job, and it runs once, after both installs.
#
# - A loaded agent is restarted through launchd explicitly. `mp daemon
#   restart` is `stop` then a start through launchd (`kickstart`, or
#   `bootstrap` for an agent launchd does not know) only when the plist bakes
#   this data directory and runs this very `mp` (docs/daemon-operations.md,
#   "Starting under the login service"); a plist that still runs another
#   binary (the version warning below) is passed over and the daemon starts
#   detached, outside launchd, while the agent's `KeepAlive {SuccessfulExit:
#   false}` keeps its own process stopped until the next login. So when the agent is
#   loaded this script runs `mp daemon stop` then `launchctl kickstart`
#   whatever the plist runs, the sequence the live launchd check took
#   (docs/tickets/0129, "after `mp daemon stop` plus `launchctl kickstart` the
#   daemon runs as launchd's child"); otherwise `mp daemon restart`, which
#   bootstraps a matching agent itself. Not `kickstart -k`: a daemon a client
#   started on demand may hold the socket while the agent's own process is
#   down, and a second launchd spawn then crash-loops (BACKLOG, "A client's
#   on-demand `mp daemon start` races the installed service").
#
# - The app is quit before the swap and relaunched after the daemon restart.
#   Quit first, so no process runs out of the bundle being replaced, and so it
#   cannot answer the restart's stop with an on-demand `mp daemon start` from
#   its old sidecar (clients/desktop/src-tauri/src/connector.rs: "Nothing
#   listening: start a daemon on demand"). Relaunch last, so its first
#   handshake meets a daemon of its own version and not the restart screen.
#   The app is quit by bundle id (`dev.mailypoppins.desktop`, tauri.conf.json
#   `identifier`) and found by the executable its Info.plist names, because
#   tauri.conf.json sets no `mainBinaryName` and the executable may be the
#   cargo name `mp-desktop` rather than the `productName`; the match is on the
#   full path under /Applications, which leaves the app's own `mp` daemon and a
#   `pnpm tauri dev` build alone.
#
# - The sidecar is the `mp` step 2 installed (`MP_SIDECAR_BIN`, the
#   alternative in clients/desktop/README.md's bundle block): one release build
#   instead of two, and the CLI, the daemon and the app's `Contents/MacOS/mp`
#   are the same bytes, so the app's version handshake cannot refuse them.
#   `--locked` matches the build `pnpm bundle` would have made itself.
#
# - The bundle is copied with `ditto` into a temp directory beside
#   /Applications/mailypoppins.app and swapped in with two `mv`s, not with
#   `rsync -a --delete` into the live bundle. A local build carries no
#   com.apple.quarantine and neither `ditto` nor `mv` adds one; `rsync` into
#   the existing directory would keep the quarantine attribute of a bundle
#   first installed from a downloaded DMG, would leave a half-copied bundle on
#   an interruption, and rewrites files under a bundle that may still be
#   mapped. `mv` gives every file a fresh inode, which is also what keeps
#   macOS's code-signing cache from killing an executable overwritten in place.
#   The only `rm -rf` is on that temp directory, which this script created.
#
# - The Homebrew cask (packaging/homebrew/mailypoppins-app.rb.tmpl) is refused
#   for the app: Homebrew owns that bundle, and the cask links the app's `mp`
#   into Homebrew's bin, a second `mp` beside ~/.cargo/bin/mp. `--no-gui` still
#   installs the CLI. A Homebrew formula `mp`, or any other `mp` ahead of
#   ~/.cargo/bin on PATH, is a warning: this script calls ~/.cargo/bin/mp by
#   path, but your shell would not.
#
# --dry-run prints every mutating command instead of running it and still runs
# the read-only checks (git status, pgrep, launchctl print, `mp --version`). It
# works on a non-macOS host as a preview; a real run refuses anywhere but macOS.
# Safe to rerun: every step converges on the same installed state.

set -euo pipefail

BUNDLE_ID="dev.mailypoppins.desktop"
SERVICE_LABEL="dev.mailypoppins.daemon"
APP_NAME="mailypoppins.app"
GUI_APP="/Applications/${APP_NAME}"
PLISTBUDDY="/usr/libexec/PlistBuddy"
QUIT_TIMEOUT=15
DAEMON_TIMEOUT=15

PULL=1
GUI=1
ALLOW_DIRTY=0
DRY_RUN=0

usage() {
  cat <<'EOF'
Usage: scripts/install-local.sh [options]

Pull, then install the CLI and TUI (`mp`) and the desktop app from source on
macOS, restart the daemon once, and relaunch the app if it was running.

Options:
  --no-pull      Build the current checkout; skip `git pull --rebase --autostash`
  --no-gui       Install only `mp`; leave the desktop app and its process alone
  --allow-dirty  Build a tree with uncommitted changes to tracked files
  --dry-run      Print every mutating command instead of running it
  -h, --help     Show this help
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --no-pull) PULL=0 ;;
    --no-gui) GUI=0 ;;
    --allow-dirty) ALLOW_DIRTY=1 ;;
    --dry-run) DRY_RUN=1 ;;
    -h | --help) usage; exit 0 ;;
    *) printf 'install-local: unknown option %s\n\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

step() { printf '\n==> %s\n' "$*"; }
info() { printf '    %s\n' "$*"; }
warn() { printf '    warning: %s\n' "$*" >&2; }
die() { printf 'install-local: %s\n' "$*" >&2; exit 1; }

# Run a mutating command, or print it under --dry-run.
run() {
  if [ "$DRY_RUN" -eq 1 ]; then
    printf '    [dry-run]'
    printf ' %q' "$@"
    printf '\n'
  else
    printf '    $'
    printf ' %q' "$@"
    printf '\n'
    "$@"
  fi
}

# A tool the run needs: fatal for a real run, a warning for a preview.
need() {
  command -v "$1" >/dev/null 2>&1 && return 0
  if [ "$DRY_RUN" -eq 1 ]; then
    warn "$1 is not on PATH; a real run would stop here"
    return 1
  fi
  die "$1 is not on PATH"
}

plist_read() { # <plist> <key>; empty when the file, the key or PlistBuddy is missing
  [ -x "$PLISTBUDDY" ] && [ -f "$1" ] || return 0
  "$PLISTBUDDY" -c "Print :$2" "$1" 2>/dev/null || true
}

version_of() { # <mp>; the last word of `mp --version`, empty when it cannot run
  [ -x "$1" ] || return 0
  "$1" --version 2>/dev/null | head -n 1 | awk '{print $NF}' || true
}

mtime() { # <file>; seconds since the epoch, GNU stat then BSD stat
  stat -c %Y "$1" 2>/dev/null || stat -f %m "$1"
}

wait_until() { # <seconds> <command...>; true once the command succeeds
  local deadline=$(($(date +%s) + $1))
  shift
  until "$@"; do
    [ "$(date +%s)" -ge "$deadline" ] && return 1
    sleep 0.5
  done
}

# ---------------------------------------------------------------------------
# Platform and paths
# ---------------------------------------------------------------------------

if [ "$(uname -s)" != "Darwin" ]; then
  [ "$DRY_RUN" -eq 1 ] || die "macOS only: it installs into /Applications and drives launchd (this host is $(uname -s)); --dry-run previews it anywhere"
  warn "not macOS: previewing only, the macOS checks answer as on a Mac with nothing installed"
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
DESKTOP="$ROOT/clients/desktop"
CARGO_BIN="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}/bin"
MP="$CARGO_BIN/mp"
[ -f "$ROOT/Cargo.toml" ] && [ -d "$ROOT/.git" ] || die "$ROOT is not the mailypoppins repository"

GUI_WAS_RUNNING=0
SWAP_DIR=""
APP_VERSION="(not installed)"
SIDECAR_VERSION=""

# The executable the installed app runs, from its Info.plist; Tauri's
# default, the productName, when there is no app yet.
gui_exe() {
  local exe
  exe="$(plist_read "$GUI_APP/Contents/Info.plist" CFBundleExecutable)"
  printf '%s' "${exe:-mailypoppins}"
}

gui_pids() {
  pgrep -f "^$GUI_APP/Contents/MacOS/$(gui_exe)( |\$)" 2>/dev/null || true
}

gui_gone() { [ -z "$(gui_pids)" ]; }

# The newest bundle under either target directory bundle.ts may have used,
# judged by its Info.plist, which every `tauri build` writes afresh.
newest_app() {
  local newest="" target_dir candidate
  for target_dir in "${CARGO_TARGET_DIR:-}" "$DESKTOP/src-tauri/target"; do
    [ -n "$target_dir" ] && [ -d "$target_dir" ] || continue
    for candidate in "$target_dir"/*/release/bundle/macos/"$APP_NAME"; do
      [ -f "$candidate/Contents/Info.plist" ] || continue
      if [ -z "$newest" ] || [ "$candidate/Contents/Info.plist" -nt "$newest/Contents/Info.plist" ]; then
        newest="$candidate"
      fi
    done
  done
  printf '%s' "$newest"
}

daemon_up() { "$MP" daemon status >/dev/null 2>&1; }

# A failure between the two `mv`s leaves no app in /Applications: put the
# previous one back. The temp directory is this script's own.
on_exit() {
  local code=$?
  if [ -n "$SWAP_DIR" ] && [ -d "$SWAP_DIR" ]; then
    if [ ! -e "$GUI_APP" ] && [ -d "$SWAP_DIR/previous.app" ]; then
      mv "$SWAP_DIR/previous.app" "$GUI_APP" && warn "restored the previous $GUI_APP"
    fi
    rm -rf "$SWAP_DIR"
  fi
  exit "$code"
}
trap on_exit EXIT

# ---------------------------------------------------------------------------
# 1. The checkout
# ---------------------------------------------------------------------------

step "repo: $ROOT"
cd "$ROOT"
need git
if ! git diff --quiet || ! git diff --cached --quiet; then
  if [ "$ALLOW_DIRTY" -eq 1 ]; then
    warn "tracked files have uncommitted changes; building them as they are (--allow-dirty)"
  else
    git status --short --untracked-files=no >&2
    die "tracked files have uncommitted changes; commit or stash them, or pass --allow-dirty"
  fi
fi
if [ "$PULL" -eq 1 ]; then
  if git rev-parse --abbrev-ref '@{u}' >/dev/null 2>&1; then
    run git pull --rebase --autostash
  else
    warn "$(git rev-parse --abbrev-ref HEAD) has no upstream; not pulling"
  fi
else
  info "not pulling (--no-pull)"
fi
COMMIT="$(git log -1 --format='%h %s')"
info "at $COMMIT"

# Refusals and warnings that do not depend on the build, before any of it.
if command -v brew >/dev/null 2>&1; then
  if [ "$GUI" -eq 1 ] && brew list --cask mailypoppins-app >/dev/null 2>&1; then
    die "the app is installed by the Homebrew cask mailypoppins-app; run \`brew uninstall --cask mailypoppins-app\` first, or pass --no-gui to install only mp"
  fi
  if brew list --formula mailypoppins >/dev/null 2>&1; then
    warn "the Homebrew formula mailypoppins also installs an mp; whichever comes first on PATH is the one your shell runs (\`brew uninstall mailypoppins\` to keep only this one)"
  fi
fi
ON_PATH="$(command -v mp 2>/dev/null || true)"
if [ -n "$ON_PATH" ] && [ "$ON_PATH" != "$MP" ]; then
  warn "\`mp\` on PATH is $ON_PATH, not $MP; this script uses $MP"
fi
# A TUI or another mp command left open keeps its old binary and restarts a
# daemon on demand the moment the restart stops it.
for pid in $(pgrep -x mp 2>/dev/null || true); do
  args="$(ps -o args= -p "$pid" 2>/dev/null || true)"
  case "$args" in
    *" daemon run"*) ;;
    "") ;;
    *) warn "mp client running (pid $pid: $args); quit it so it does not race the daemon restart" ;;
  esac
done

# ---------------------------------------------------------------------------
# 2. The CLI and the TUI
# ---------------------------------------------------------------------------

step "cli: cargo install --path . --locked (mp, the CLI and the TUI)"
need cargo || true
run cargo install --path . --locked
MP_VERSION="$(version_of "$MP")"
[ -n "$MP_VERSION" ] || [ "$DRY_RUN" -eq 1 ] || die "$MP --version did not answer"
info "$MP: mailypoppins ${MP_VERSION:-(not installed)}"

# ---------------------------------------------------------------------------
# 3. The desktop app
# ---------------------------------------------------------------------------

if [ "$GUI" -eq 1 ]; then
  step "gui: pnpm bundle in clients/desktop, sidecar $MP"
  need pnpm || true
  need node || true
  if [ ! -d "$DESKTOP/node_modules" ] || [ "$DESKTOP/pnpm-lock.yaml" -nt "$DESKTOP/node_modules/.modules.yaml" ]; then
    run pnpm --dir "$DESKTOP" install --frozen-lockfile
  else
    info "node_modules is current with pnpm-lock.yaml"
  fi

  PREVIOUS_APP="$(newest_app)"
  PREVIOUS_MTIME=0
  [ -z "$PREVIOUS_APP" ] || PREVIOUS_MTIME="$(mtime "$PREVIOUS_APP/Contents/Info.plist")"
  run env MP_SIDECAR_BIN="$MP" pnpm --dir "$DESKTOP" bundle
  NEW_APP="$(newest_app)"
  if [ "$DRY_RUN" -eq 1 ]; then
    NEW_APP="${NEW_APP:-<target dir>/<target>/release/bundle/macos/$APP_NAME}"
  else
    [ -n "$NEW_APP" ] || die "pnpm bundle produced no $APP_NAME under ${CARGO_TARGET_DIR:+$CARGO_TARGET_DIR or }$DESKTOP/src-tauri/target"
    if [ "$(mtime "$NEW_APP/Contents/Info.plist")" -le "$PREVIOUS_MTIME" ]; then
      die "pnpm bundle wrote no new $APP_NAME; the newest, $NEW_APP, predates this run"
    fi
  fi
  info "built $NEW_APP"

  if ! gui_gone; then
    GUI_WAS_RUNNING=1
    step "gui: quitting the running app ($(gui_exe), pid $(gui_pids | tr '\n' ' '| sed 's/ $//'))"
    run osascript -e "quit app id \"$BUNDLE_ID\"" || warn "osascript could not ask the app to quit; falling back to signals"
    if [ "$DRY_RUN" -eq 0 ] && ! wait_until "$QUIT_TIMEOUT" gui_gone; then
      warn "still running after ${QUIT_TIMEOUT}s; sending SIGTERM"
      # shellcheck disable=SC2046 # one pid per word
      run kill $(gui_pids)
      if ! wait_until 5 gui_gone; then
        warn "still running; sending SIGKILL"
        # shellcheck disable=SC2046
        run kill -9 $(gui_pids)
        wait_until 5 gui_gone || die "could not stop the running app"
      fi
    fi
  else
    info "the app is not running"
  fi

  step "gui: installing $GUI_APP"
  [ -w /Applications ] || [ "$DRY_RUN" -eq 1 ] || die "/Applications is not writable by $(id -un)"
  if [ "$DRY_RUN" -eq 1 ]; then
    SWAP_PREVIEW="/Applications/.mailypoppins-install.XXXXXX"
    run mktemp -d "$SWAP_PREVIEW"
    run ditto "$NEW_APP" "$SWAP_PREVIEW/$APP_NAME"
    if [ -e "$GUI_APP" ]; then
      run mv "$GUI_APP" "$SWAP_PREVIEW/previous.app"
    fi
    run mv "$SWAP_PREVIEW/$APP_NAME" "$GUI_APP"
    run rm -rf "$SWAP_PREVIEW"
  else
    SWAP_DIR="$(mktemp -d "/Applications/.mailypoppins-install.XXXXXX")"
    run ditto "$NEW_APP" "$SWAP_DIR/$APP_NAME"
    if [ -e "$GUI_APP" ]; then
      run mv "$GUI_APP" "$SWAP_DIR/previous.app"
    fi
    run mv "$SWAP_DIR/$APP_NAME" "$GUI_APP"
    run rm -rf "$SWAP_DIR"
    SWAP_DIR=""
  fi
  APP_VERSION="$(plist_read "$GUI_APP/Contents/Info.plist" CFBundleShortVersionString)"
  APP_VERSION="${APP_VERSION:-(unknown)}"
  SIDECAR_VERSION="$(version_of "$GUI_APP/Contents/MacOS/mp")"
  info "$GUI_APP: version $APP_VERSION, its mp ${SIDECAR_VERSION:-(none)}"
  if [ "$DRY_RUN" -eq 0 ] && [ "$SIDECAR_VERSION" != "$MP_VERSION" ]; then
    warn "the app's mp ($SIDECAR_VERSION) is not $MP ($MP_VERSION); the app will show the restart screen"
  fi
else
  info "skipping the desktop app (--no-gui)"
  if ! gui_gone; then
    # A running app restarts a daemon on demand from its own bundled mp the
    # moment the socket goes quiet, so it would race the daemon restart below:
    # either the old version keeps serving while the launchd agent crash-loops
    # on the held socket, or the app shows the restart screen and its Restart
    # starts the old daemon outside launchd. Quitting it first is the only
    # order that works.
    die "the app is running (pid $(gui_pids | tr '\n' ' ' | sed 's/ $//')); quit it before a --no-gui run, or drop --no-gui so the script quits it"
  fi
fi

# ---------------------------------------------------------------------------
# 4. The daemon, once, after both installs
# ---------------------------------------------------------------------------

UID_NUM="$(id -u)"
SERVICE_PLIST="$HOME/Library/LaunchAgents/$SERVICE_LABEL.plist"
if command -v launchctl >/dev/null 2>&1 && launchctl print "gui/$UID_NUM/$SERVICE_LABEL" >/dev/null 2>&1; then
  step "daemon: restarting through launchd ($SERVICE_LABEL)"
  BAKED="$(plist_read "$SERVICE_PLIST" ProgramArguments:0)"
  BAKED_VERSION="$(version_of "$BAKED")"
  if [ -z "$BAKED" ]; then
    warn "could not read the binary $SERVICE_PLIST runs"
  elif [ "$BAKED_VERSION" != "$MP_VERSION" ] && [ "$DRY_RUN" -eq 0 ]; then
    warn "the agent runs $BAKED (${BAKED_VERSION:-missing}), not $MP_VERSION; \`$MP daemon install-service --force\` points it at $MP"
  fi
  run "$MP" daemon stop
  run launchctl kickstart "gui/$UID_NUM/$SERVICE_LABEL"
  if [ "$DRY_RUN" -eq 0 ] && ! wait_until "$DAEMON_TIMEOUT" daemon_up; then
    warn "no daemon answered ${DAEMON_TIMEOUT}s after the kickstart; \`$MP daemon logs\` has the reason"
  fi
else
  step "daemon: mp daemon restart (agent not loaded; a matching plist is bootstrapped)"
  run "$MP" daemon restart
fi

DAEMON_STATE="not running"
if daemon_up; then
  STATUS_JSON="$("$MP" daemon status --json 2>/dev/null || true)"
  DAEMON_VERSION="$(printf '%s' "$STATUS_JSON" | sed -n 's/.*"app_version":"\([^"]*\)".*/\1/p')"
  DAEMON_PID="$(printf '%s' "$STATUS_JSON" | sed -n 's/.*"pid":\([0-9]*\).*/\1/p')"
  DAEMON_PPID="$(ps -o ppid= -p "${DAEMON_PID:-0}" 2>/dev/null | tr -d ' ' || true)"
  DAEMON_STATE="running, version ${DAEMON_VERSION:-?}, pid ${DAEMON_PID:-?}"
  [ "$DAEMON_PPID" = "1" ] && DAEMON_STATE="$DAEMON_STATE, under launchd"
  if [ "$DRY_RUN" -eq 0 ] && [ -n "$MP_VERSION" ] && [ "$DAEMON_VERSION" != "$MP_VERSION" ]; then
    warn "the daemon runs $DAEMON_VERSION, not $MP_VERSION"
  fi
  # Health only against a daemon that answers: on a stopped one it fails too and
  # the stop would be reported twice.
  "$MP" daemon health || warn "\`mp daemon health\` reported a problem"
else
  [ "$DRY_RUN" -eq 1 ] || warn "no daemon answers; \`$MP daemon logs\` has the reason"
fi

if [ "$GUI" -eq 1 ] && [ "$GUI_WAS_RUNNING" -eq 1 ]; then
  step "gui: relaunching $GUI_APP"
  # By path: `open -a mailypoppins` asks LaunchServices, which also knows the
  # bundle just built under the target directory and may open that one.
  run open "$GUI_APP"
fi

# ---------------------------------------------------------------------------
# 5. Summary
# ---------------------------------------------------------------------------

if [ "$DRY_RUN" -eq 1 ]; then
  step "summary (dry run, nothing was changed)"
else
  step "summary"
fi
info "commit:  $COMMIT"
info "mp:      ${MP_VERSION:-(not installed)} ($MP)"
if [ "$GUI" -eq 1 ]; then
  info "app:     $APP_VERSION ($GUI_APP), its mp ${SIDECAR_VERSION:-(none)}"
else
  info "app:     not touched (--no-gui)"
fi
info "daemon:  $DAEMON_STATE"
