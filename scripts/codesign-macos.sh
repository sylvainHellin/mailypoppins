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
