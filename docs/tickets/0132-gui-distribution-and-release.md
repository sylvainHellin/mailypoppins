---
id: 0132
title: Phase 10 of the native GUI, distribution and release
type: chore
priority: next
status: open
created: 2026-09-25
---

Last ticket of the GUI half of the [native GUI plan](../plans/native-gui.md), section "Phase 10: Distribution and release".
Blocked on #0131.

## Work

- Extend the release workflow with signed and notarized macOS app bundles and DMGs; the Developer ID account and CI secrets are #0012.
- Bundle the matching `mp` executable inside `Mailypoppins.app`, with a supported way to expose it on PATH.
- Keep the standalone macOS and Linux CLI archives and the Homebrew installation working.
- Make the GUI and the bundled daemon complete the version handshake, including the mismatch restart path.
- Clean-install, upgrade, restart, uninstall and quarantine smoke tests.
- Update [docs/release-process.md](../release-process.md), the website and the installation instructions.

## Exit gate

- A clean macOS machine can install the app, start the daemon, use the GUI, run `mp`, restart after a mismatch, and uninstall cleanly.
- Standalone CLI releases remain functional on every existing target.
- Signing and notarization pass in CI.
