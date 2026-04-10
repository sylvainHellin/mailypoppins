#!/usr/bin/env bash
# Re-sign the installed `email` binary with a stable code-signing identity
# so the macOS Keychain ACL persists across rebuilds.
#
# Without this step, every `cargo install --path .` produces an ad-hoc signed
# binary with a new cdhash, which the Keychain treats as a different app and
# re-prompts for SMTP/IMAP password access on the next TUI launch.
#
# One-time setup: see CONTRIBUTING.md for instructions on creating the
# self-signed certificate.
#
# Usage: ./scripts/codesign-macos.sh
# Or via: ./scripts/install.sh (which calls this after `cargo install`)
#
# Env vars:
#   EMAIL_CODESIGN_IDENTITY  Code-signing identity name (default: email-dev)
#   EMAIL_BINARY_PATH        Path to binary (default: $CARGO_HOME/bin/email)

set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    exit 0
fi

IDENTITY="${EMAIL_CODESIGN_IDENTITY:-email-dev}"
BINARY="${EMAIL_BINARY_PATH:-${CARGO_HOME:-$HOME/.cargo}/bin/email}"

if [[ ! -x "$BINARY" ]]; then
    echo "codesign-macos: binary not found at $BINARY" >&2
    exit 1
fi

# Use `find-identity -p codesigning` WITHOUT `-v`. A self-signed dev cert is
# not trust-anchored for the codeSign policy, so `-v` would filter it out
# (CSSMERR_TP_NOT_TRUSTED) even though `codesign --sign` can use it just fine.
# We only need: (1) a cert + matching private key, (2) the Code Signing EKU.
# Both are satisfied by Certificate Assistant's "Code Signing" type without
# any trust override.
if ! security find-identity -p codesigning | grep -q "\"$IDENTITY\""; then
    cat >&2 <<EOF
codesign-macos: code-signing identity "$IDENTITY" not found in login keychain.

One-time setup (see CONTRIBUTING.md for full instructions):
  1. Open Keychain Access
  2. Certificate Assistant > Create a Certificate
  3. Name: $IDENTITY, Identity Type: Self Signed Root, Type: Code Signing
  4. "Let me override defaults" > set validity to 3650 days
  5. Click Create (the cert lands in the login keychain)

No trust override is required -- the cert only needs a matching private key
and the Code Signing EKU, both of which Certificate Assistant sets by default.

Or set EMAIL_CODESIGN_IDENTITY to point at an existing identity.
EOF
    exit 1
fi

codesign --force --sign "$IDENTITY" --identifier "email" "$BINARY"

DR=$(codesign -d -r - "$BINARY" 2>&1 | grep "designated =>" || true)
echo "codesign-macos: signed $BINARY"
if [[ -n "$DR" ]]; then
    echo "  $DR"
fi

# ---------------------------------------------------------------------------
# Reset Keychain ACLs for email-cli items.
#
# Problem: every time the user clicks "Always Allow" in the Keychain prompt,
# macOS stores the binary's cdhash (unique per build) rather than the
# certificate-based designated requirement.  After the next rebuild the
# cdhash no longer matches and the prompt returns.
#
# Fix: delete each email-cli item and re-create it with
#   security add-generic-password -T <signed-binary>
# When -T is used programmatically, macOS records the binary's designated
# requirement (certificate-based, stable across rebuilds).  We also add
# -T /usr/bin/security so that future resets can read without prompting.
#
# First run after applying this fix may show one Keychain prompt per item
# (to let /usr/bin/security read the current value).  After that, all
# subsequent installs are fully silent.
#
# Set EMAIL_SKIP_KEYCHAIN_RESET=1 to skip this step.
# ---------------------------------------------------------------------------
if [[ "${EMAIL_SKIP_KEYCHAIN_RESET:-}" == "1" ]]; then
    exit 0
fi

SERVICE="email-cli"

# Collect account names for email-cli items from the login keychain.
# In the dump format, 0x00000007 holds the service name and appears right
# before the "acct" line within the same item block.
accounts=$(security dump-keychain 2>/dev/null | awk '
    /0x00000007 <blob>="email-cli"/ { found = 1; next }
    found && /"acct"<blob>="/ {
        gsub(/.*"acct"<blob>="/, "")
        gsub(/".*/, "")
        print
        found = 0
    }
')

if [[ -z "$accounts" ]]; then
    exit 0
fi

reset=0
skipped=0
while IFS= read -r acct; do
    # Try to read the current password.  On the very first run this may
    # trigger a Keychain prompt for /usr/bin/security.
    pw=$(security find-generic-password -s "$SERVICE" -a "$acct" -w 2>/dev/null) || {
        skipped=$((skipped + 1))
        continue
    }

    # Delete the item (removes all stale cdhash ACL entries).
    security delete-generic-password -s "$SERVICE" -a "$acct" >/dev/null 2>&1 || {
        skipped=$((skipped + 1))
        continue
    }

    # Re-create with the signed binary and /usr/bin/security as trusted apps.
    # -T records the designated requirement (cert-based, stable across rebuilds).
    if security add-generic-password \
            -s "$SERVICE" -a "$acct" -w "$pw" \
            -T "$BINARY" -T "/usr/bin/security" 2>/dev/null; then
        reset=$((reset + 1))
    else
        # Fallback: re-create without app restriction so the password isn't lost.
        security add-generic-password -s "$SERVICE" -a "$acct" -w "$pw" -A 2>/dev/null || true
        skipped=$((skipped + 1))
    fi
done <<< "$accounts"

if [[ $reset -gt 0 ]]; then
    echo "codesign-macos: reset Keychain ACL for $reset item(s)"
fi
if [[ $skipped -gt 0 ]]; then
    echo "codesign-macos: skipped $skipped item(s) (run again after granting access)"
fi
