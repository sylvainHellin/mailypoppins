#!/usr/bin/env bash
set -euo pipefail

# Build the website and upload it to the OVH free hosting of mailypoppins.com.
# The plan has SFTP but no SSH, so this uploads with sftp instead of rsync and
# does not delete remote files that the build no longer produces: remove those
# by hand (sftp, `rm www/<path>`) when a page is retired.
# The password is read from Proton Pass at run time through SSH_ASKPASS.

OVH_HOST="${OVH_HOST:-ftp.cluster129.hosting.ovh.net}"
OVH_USER="${OVH_USER:-mailypg}"
OVH_PATH="${OVH_PATH:-www}"
PASS_VAULT="${PASS_VAULT:-API Keys and tokens}"
PASS_ITEM="${PASS_ITEM:-ftp-ovh-mailypoppins}"

root="$(git rev-parse --show-toplevel)"

echo ":: Building site..."
(cd "$root/website" && pnpm run build)

askpass="$(mktemp)"
trap 'rm -f "$askpass"' EXIT
cat >"$askpass" <<ASK
#!/bin/sh
exec pass-cli item view --vault-name "$PASS_VAULT" --item-title "$PASS_ITEM" --field Password
ASK
chmod 700 "$askpass"

echo ":: Uploading to ${OVH_USER}@${OVH_HOST}:${OVH_PATH}..."
# BatchMode=no: `sftp -b` turns BatchMode on, which disables password auth.
# accept-new: a machine that never reached the host would otherwise wait
# forever on the host-key prompt, which the askpass cannot answer.
printf 'lcd %s\ncd %s\nput -r *\n' "$root/website/dist" "$OVH_PATH" |
  SSH_ASKPASS="$askpass" SSH_ASKPASS_REQUIRE=force DISPLAY="${DISPLAY:-:0}" \
    sftp -q -o BatchMode=no -o PreferredAuthentications=password \
    -o StrictHostKeyChecking=accept-new -b - \
    "${OVH_USER}@${OVH_HOST}"

echo ":: Done. Site deployed to https://mailypoppins.com"
