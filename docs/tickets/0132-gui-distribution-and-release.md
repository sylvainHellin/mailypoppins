---
id: 0132
title: M6 of the native GUI, distribution and release
type: chore
priority: next
status: open
created: 2026-09-25
---

Last ticket of the [native GUI plan](../plans/native-gui.md), milestone "M6: signing, notarisation and bundling".
Blocked on #0130, since M6 follows M5.

## Work

- Extend the release workflow with signed and notarized macOS app bundles and DMGs; the Developer ID account and CI secrets are #0012.
- Bundle the matching `mp` executable inside `Mailypoppins.app`, with a supported way to expose it on PATH.
- Keep the standalone macOS and Linux CLI archives and the Homebrew installation working.
- Make the GUI and the bundled daemon complete the version handshake, including the mismatch restart path.
- Clean-install, upgrade, restart, uninstall and quarantine smoke tests.
- Update [docs/release-process.md](../release-process.md), the website and the installation instructions.

## Landed (2026-10-01), everything but signing

- The `desktop-macos` job in `.github/workflows/release.yml` builds `aarch64-apple-darwin` and `x86_64-apple-darwin` on a `v*` tag and attaches `mailypoppins-desktop-<target>.dmg` and `.app.tar.gz` with `.sha256` files; plain `pnpm bundle` (`tauri build`) rather than tauri-action, for the reasons in [release-process.md](../release-process.md), "The desktop app".
- `mp` is a `bundle.externalBin` sidecar, named in `clients/desktop/src-tauri/tauri.bundle.conf.json` rather than `tauri.conf.json` so `tauri dev` and `cargo test` need no staged binary; `clients/desktop/scripts/bundle.ts` builds and stages it, and the bundle lands as `mailypoppins.app/Contents/MacOS/mp`, the connector's first candidate.
- The signing step is a marked, commented-out `env:` block naming the six `APPLE_*` secrets; Tauri's bundler reads them itself.
- The version handshake: the app refuses a daemon whose `app_version` differs from the `--version` of the `mp` it would start, with the restart screen, at the first connect and on a reconnect ([rust-layer.md](../../clients/desktop/docs/rust-layer.md), "Conventions").
- `mp` on PATH: the symlink into `/Applications/mailypoppins.app/Contents/MacOS/mp` is documented, and `packaging/homebrew/mailypoppins-app.rb.tmpl` is the cask skeleton, unpublished until the signed build.
- The app is `mailypoppins.app` (the `productName`), not `Mailypoppins.app`.

## Exit gate

- A clean macOS machine can install the app, start the daemon, use the GUI, run `mp`, restart after a mismatch, and uninstall cleanly.
  - 2026-10-01, on the development Mac, unsigned local build: `pnpm bundle` produced the `.app` (35.75 MiB) with `Contents/MacOS/mp` 0.10.0 and the DMG (14.22 MiB), and `pnpm bundle --target x86_64-apple-darwin` the cross-compiled pair (38.35 and 15.03 MiB, both executables x86_64), which the upload step's packaging commands turned into the four release assets and their checksums; the `.app` copied to `/Applications` (as `mailypoppins-r-smoke.app`, since a `mailypoppins.app` was installed and running), launched with `MP_DESKTOP_MP_BIN` unset, no `mp` on PATH and scratch data and config directories, ran `Contents/MacOS/mp daemon start`, and the bundled daemon answered and bootstrapped.
  - Quitting and relaunching reconnected to the same daemon instance; with `MP_DESKTOP_MP_BIN` on a wrapper reporting 99.0.0, the connect was refused as the version mismatch naming both versions.
  - Not yet run: the Restart button against a daemon of a really different version (needs two releases), the GUI use beyond the bootstrap, the uninstall (documented: `mp daemon stop`, quit, delete the app and the symlink), the DMG mounted, a clean machine, and the quarantine path of a downloaded DMG.
- Standalone CLI releases remain functional on every existing target.
  - The `upload-assets` and `homebrew-tap` jobs are unchanged; the new job does not gate them. Proven only by the next tag.
- Signing and notarization pass in CI.
  - Open, #0012.
