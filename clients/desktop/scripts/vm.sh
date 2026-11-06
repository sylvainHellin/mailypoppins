#!/bin/sh
# Drive the desktop app inside a Lume macOS VM (PERSO-271 part B).
# Usage, with the VM running (`lume run <vm> --no-display`):
#   scripts/vm.sh ssh 'command'           run a shell command in the guest
#   scripts/vm.sh call <tool> [args...]   cua-driver tool, JSON arguments on stdin
#   scripts/vm.sh shot <name>             guest screenshot to .sync/gui-vm/<name>.png
#   scripts/vm.sh vnc <vncdo args...>     real input through the VM's VNC port
# The VM defaults to mp-tahoe-work; set MP_VM to pick another.
# `vnc` is the only input macOS's secure dialogs (the admin password prompt
# of Privacy & Security) accept; cua-driver's synthetic events reach the
# webview and ordinary windows in the background.
set -eu
VM=${MP_VM:-mp-tahoe-work}
KEY="$HOME/.lume/ssh/id_ed25519"
SSH_OPTS="-i $KEY -o IdentitiesOnly=yes -o BatchMode=yes -o UserKnownHostsFile=$HOME/.lume/ssh/known_hosts"
ip() { lume get "$VM" --format json | jq -r '.[0].ipAddress'; }
# CuaDriver.app does not start at guest login; `open -g` starts its daemon
# without fronting anything, and is a no-op once it runs.
DRIVER='pgrep -q -f "cua-driver serve" || { open -g -a /Applications/CuaDriver.app; sleep 3; }; ~/.local/bin/cua-driver call'
cmd=${1:?usage: vm.sh ssh|call|shot|vnc ...}
shift
case "$cmd" in
  ssh) exec ssh $SSH_OPTS "lume@$(ip)" "$@" ;;
  call) exec ssh $SSH_OPTS "lume@$(ip)" "$DRIVER $*" ;;
  shot)
    out="$(git rev-parse --show-toplevel)/.sync/gui-vm/${1:?name}.png"
    mkdir -p "$(dirname "$out")"
    host=$(ip)
    echo '{}' | ssh $SSH_OPTS "lume@$host" "$DRIVER get_desktop_state --screenshot-out-file /tmp/shot.png" >/dev/null
    scp -q $SSH_OPTS "lume@$host:/tmp/shot.png" "$out"
    echo "$out"
    ;;
  vnc)
    url=$(lume get "$VM" --format json | jq -r '.[0].vncUrl')
    pw=$(echo "$url" | sed -E 's#vnc://:([^@]*)@.*#\1#')
    port=$(echo "$url" | sed -E 's#.*:([0-9]+)$#\1#')
    cd /var/tmp
    uv run -q --with vncdotool vncdo -s "127.0.0.1::$port" -p "$pw" "$@" 2>&1 \
      | grep -v -e Deprecation -e encryptor || true
    ;;
  *) echo "unknown command: $cmd" >&2; exit 2 ;;
esac
