---
id: 0139
title: Desktop app updates itself from GitHub Releases
type: feature
priority: next
status: open
created: 2026-10-04
---

Proposed 2026-10-04, awaiting Sylvain's review of the design.

Plane PERSO-85.
It extends [#0132](0132-gui-distribution-and-release.md), whose release job ships the unsigned DMGs this ticket teaches to update themselves.
The external facts are in `.agents/research/2026-10-04-tauri-updater-research.md`; every repo fact below was re-read for this ticket, with the file named.

## Goal

- Install the app once from the DMG; every later version arrives through the app.
- The app shows that an update is available, and the palette and the app menu install it.
- An optional setting installs updates on its own, applied at the next launch.

## Constraints found in the repo

- The app and the DMGs are unsigned: the `desktop-macos` job in `.github/workflows/release.yml` carries the six `APPLE_*` secrets as a commented-out `env:` block, waiting on [#0012](0012-apple-developer-id-signing.md), and the Apple enrolment is Plane PERSO-84.
- The release workflow is hand-written and does not use tauri-action: `create-release` makes the release with the changelog notes, and `desktop-macos` runs `pnpm bundle --target $TARGET -- --ci --bundles app,dmg` for `aarch64-apple-darwin` on `macos-latest`; there is no Intel build.
- `create-release` runs `gh release create` without `--draft`, so a release is public and "latest" from its first minute, before any asset is attached.
- The upload step builds its own `mailypoppins-desktop-$TARGET.app.tar.gz` with `tar -czf ... -C "$bundle/macos" mailypoppins.app` and a `.sha256` beside it; the archive's top-level entry is `mailypoppins.app/`, which is the layout the updater expects.
- `pnpm bundle` (`clients/desktop/scripts/bundle.ts`) builds `mp`, stages it as `src-tauri/binaries/mp-<target>`, and runs `tauri build` with `--config tauri.bundle.conf.json`, the file that holds `bundle.externalBin: ["binaries/mp"]`, so `tauri dev` and `cargo test` need no staged sidecar.
- The bundled `mp` lands as `mailypoppins.app/Contents/MacOS/mp`, the first candidate of `resolve_mp_binary` after `MP_DESKTOP_MP_BIN` (`clients/desktop/src-tauri/src/connector.rs`, module doc).
- The app starts its daemon with that binary (`mp daemon start` as a child process), so the daemon runs from inside the bundle the updater replaces.
- The version handshake refuses a daemon whose `app_version` differs from the `--version` of the `mp` the app would start (`check_version`, `version_mismatch` in `connector.rs`), with the blocking restart screen whose Restart runs `restart_daemon_blocking`, that is `mp daemon restart` with the same binary.
  The probe is cached by the binary's path, size and modification time, so a replaced binary is read again.
- `tauri.conf.json` has `"version": "0.1.0"` and `identifier: dev.mailypoppins.desktop`; `clients/desktop/src-tauri/Cargo.toml` is also `0.1.0`.
- The CSP's `connect-src` is `ipc: http://ipc.localhost`, and `capabilities/default.json` grants core, window destroy, the open dialog and `opener:allow-open-url` for `https://*`, `http://*` and `mailto:*`.
- `desktop.json` (`clients/desktop/src-tauri/src/settings.rs`) is a flat JSON object of string values with four snake_case keys (`editor`, `theme`, `reader_mode`, `editor_colors`); a key outside `SettingKey` is refused.
- The app has a native App menu (`src-tauri/src/menu.rs`) with "Settings…" on Cmd+, and an About item.

### Version stamping

`bundle.ts` stamps the version: it reads `mp --version` off the staged binary (for a cross-compiled one that cannot run, `cargo pkgid` of the root crate) and passes `--config {"version": "<that>"}` to `tauri build`.
The bundle's version is therefore the root `Cargo.toml` version, not the tag.
Nothing in the workflow checks that the tag matches it; `docs/release-process.md` step 1 asks for the bump by hand, and the changelog extraction only warns on a missing section.
The updater compares `latest.json`'s `version` with the running app's `package_info().version`.
That this reads the `--config` override rather than the file's `0.1.0` is inferred: the Tauri CLI hands the merged config to the build, and #0132's local bundle carried 0.10.0, but nobody has printed `package_info().version` from a bundled app yet.
The desktop crate's own `CARGO_PKG_VERSION` stays `0.1.0` and is what the GUI announces as its `app_version` in `initialize`; the handshake compares the daemon with the bundled `mp`, so that number does not matter here.

## Design

Three stages.
Stage 1 needs no Apple account and ships first.

### Stage 1: check, indicator, install on request

Plugin and signing:

- `tauri-plugin-updater` with a minisign keypair from `pnpm tauri signer generate`; the signature is the updater's own and has nothing to do with Apple signing.
- The check and the install live in Rust commands, so the frontend needs no `updater:*` permission and the CSP stays as it is; app-defined commands need no capability entry, as the existing ones show.
- Turn on `require_signed_version` if the plugin's config exposes it (the research note read it in `config.rs`), so a tampered manifest cannot pair a new version number with an old signed archive.
- The relaunch is `AppHandle::restart` or `request_restart` from tauri core (both in tauri 2.12, verified on [docs.rs](https://docs.rs/tauri/2.12.0/tauri/struct.AppHandle.html)), so `tauri-plugin-process` is not needed while the flow lives in Rust; it would only provide the JS `relaunch()`.

When the app checks:

- Once, about 10 s after startup, and at most once per 24 h.
- `last_check` and `skipped_version` live in a small state file in the app data directory (`update-state.json`), since `desktop.json` holds presentation choices and its commands accept only `SettingKey` names.
- Never in a debug build, under `MP_DESKTOP_FIXTURE=1`, or when the executable does not run from a `.app` bundle: a dev build is `0.1.0` and would always see an update.
- A failed silent check is logged and shown nowhere; a failed manual check says why on the notice line.

Where the user sees it, following [shell.md](../../clients/desktop/docs/shell.md):

- The indicator is an entry at the sidebar's foot, beside Contacts, Calendar, Settings and Activity ("Views", entry points), shown only while an update is known: "Update to 0.12.0".
  The `StatusRegion` banners are for the connection and would be too loud for this.
- The palette has "Check for updates" always, and "Update to vX.Y.Z" while one is known.
- The App menu gets "Check for Updates…" under About, where a Mac app keeps it.
- Settings, General, gets an Updates line: the running version, the last check, and a "Check now" button.
- A manual check that finds nothing says "mailypoppins 0.11.0 is up to date" on the notice line.

The install flow:

1. The user picks the indicator, the palette command or the menu item.
2. `update_install` runs `download_and_install` and reports `Started{content_length}`, the summed chunks and `Finished` over a `tauri::ipc::Channel`.
   The activity area shows it as a running card, "Downloading mailypoppins 0.12.0…" with a progress bar, as it shows a contact rebuild.
3. The daemon keeps running during the download and the swap: the plugin renames the running bundle away and the new one into place, and a running process keeps its open binary.
   A failed download or a refused signature therefore leaves the app and the daemon as they were.
4. On success the card asks "Restart now" or "Later".
5. Restart now runs the existing leave question first (open drafts in the terminal pane, as a window close does), then stops the daemon with `daemon.stop` over the app's session, then calls `request_restart`; the new app starts a daemon of its own version on its first connect.
   A TUI or a CLI attached to the daemon loses it for those seconds, as it does on any `mp daemon restart`.
6. Later keeps the old app and the old daemon until the user quits; the indicator becomes "Restart to finish the update".
7. Any failure (network, signature, permissions) says why on the card and offers "Open the release page", which opens `https://github.com/sylvainHellin/mailypoppins/releases/latest` through the opener plugin already granted for `https://*`.

This order differs from "stop the daemon, then install" in the brief: stopping first would leave the user without a daemon when the download fails.

### Stage 2: install automatically

- A `desktop.json` key `auto_update`, `on` or `off`, unset is `off`, in the flat snake_case style of the four keys there (a dotted `updates.auto_install` would be the file's first nested name).
- Settings, General, shows it as a toggle under the Updates line, and the palette gets "Updates: install automatically" and "Updates: ask first".
- When on, the startup check downloads the update in the background and keeps the verified bytes, with no prompt.
- The install (`Update::install(bytes)`) runs in the app's exit path, so the bundle never changes under a running session; the next launch is the new version.
- If the bundle's parent directory is not writable, the automatic route stands down and falls back to the indicator, since the plugin's admin-password fallback has no place at quit.
- After an install the new app's first connect meets the old daemon.
  The app records `{from, to}` in `update-state.json` at install, and a version mismatch whose daemon version is `from` restarts the daemon without the blocking screen; any other mismatch still shows it.

### Stage 3: after #0012

- Uncomment the `APPLE_*` block; `APPLE_SIGNING_IDENTITY` then replaces the ad-hoc `"-"` and `tauri build` signs, notarises and staples.
- Publish the cask from `packaging/homebrew/mailypoppins-app.rb.tmpl` with `auto_updates true`, so `brew upgrade` leaves the self-updating app alone unless `--greedy` is passed (the research note marks this reading of the Cask Cookbook unverified).
- Keep the same minisign key and `pubkey`: an installed app rejects every update signed by another key.
- Test the move from an ad-hoc build to the Developer ID build through the updater once, since Keychain items and Automation grants bound to the ad-hoc identity may ask again one last time.

## Unsigned macOS: the verdict and its evidence

Self-update works without an Apple account; the first install from a browser download still needs "Open Anyway" or `xattr -dr com.apple.quarantine`, as the website says today.

- No Apple check in the install path.
  Verified by the research note in the plugin's source: the macOS `install_inner` untars into a temp dir, renames the running bundle to a backup, renames the new one into place and `touch`es it, with no `codesign`, `spctl` or notarisation step ([updater.rs](https://github.com/tauri-apps/plugins-workspace/blob/v2/plugins/updater/src/updater.rs)).
  The minisign check (`verify_signature`) is the only gate.
- No quarantine on the replaced bundle.
  Inferred: the plugin's HTTP client and `tar` set no `com.apple.quarantine`, which browsers and Homebrew cask downloads do; one user report says the same ([nodal-agentos #12](https://github.com/Mazp17/nodal-agentos/issues/12)), and two others report the minisign updater working with Apple-unsigned builds ([VATUSA/OIS #535](https://github.com/VATUSA/OIS/issues/535), [gdom PR #48](https://github.com/phucrio/gdom/pull/48)).
  No Tauri document says so; the Mac test below settles it.
- Ad-hoc identity.
  Verified in the Tauri docs: `signingIdentity: "-"` is ad-hoc signing, useful on Apple Silicon, and does not spare the user the Privacy & Security step ([macOS signing](https://v2.tauri.app/distribute/sign/macos/)); a maintainer recommends it against the "damaged" dialog on CI builds ([tauri-action #824](https://github.com/tauri-apps/tauri-action/issues/824), [tauri #8763](https://github.com/tauri-apps/tauri/issues/8763)).
  Inferred: every ad-hoc build has a new code identity, so grants tied to the old one may prompt again after each update (one report, VATUSA/OIS #535).
  For mailypoppins two grants are exposed: the Keychain, but only for an account that chose `secrets_backend = "keyring"`, since the default is `encrypted-file` (`crates/mp-core/src/secrets.rs`, `#[default] EncryptedFile`) and the daemon, not the GUI, reads secrets; and the Automation grant for Terminal.app, which the app drives through `osascript` as its last-resort terminal for `$EDITOR` (`src-tauri/src/editor.rs`).
- `/Applications` not writable.
  Verified in the source by the research note: when the rename is refused, the plugin falls back to an AppleScript `do shell script ... with administrator privileges`, an admin password dialog.
  Inferred: a drag-installed app in `/Applications` is owned by the admin user who dragged it, so an admin account sees no prompt and a standard account does ([tauri #8372](https://github.com/tauri-apps/tauri/issues/8372)).
- No bundle-identifier check.
  Verified in the source by the research note: the plugin replaces the running bundle with whatever the signed archive holds.
  The guard is the release process and the minisign key; `identifier` and `productName` stay fixed, or the cask's `app "mailypoppins.app"` breaks.

## Release workflow changes

- [x] Generate the key once: `pnpm tauri signer generate -w ~/.tauri/mailypoppins.key`, with a password.
- [x] Add the repository secrets `TAURI_SIGNING_PRIVATE_KEY` (the key's content) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, exposed as `env:` on the "Build the app bundle and DMG with the mp sidecar" step beside the commented `APPLE_*` block.
- [x] Turn on `bundle.createUpdaterArtifacts: true` only where the key exists: `bundle.ts` adds it to its `--config` when `TAURI_SIGNING_PRIVATE_KEY` is set, so a local `pnpm bundle` without the key keeps working (that the build fails without the key is inferred from the docs, to check once).
- [x] Put `bundle.macOS.signingIdentity: "-"` in `tauri.bundle.conf.json`, and confirm with `codesign -dv` on a CI artifact.
- [x] Put `plugins.updater` in `tauri.conf.json`: `pubkey` (the public key's content) and `endpoints: ["https://github.com/sylvainHellin/mailypoppins/releases/latest/download/latest.json"]`; owner and repository are from `git remote -v` and the cask template's URL.
- [x] Stamp and check the version: `bundle.ts` already stamps the root crate's version, and the new manifest job fails when `${GITHUB_REF_NAME#v}` differs from it, so a tag without the `Cargo.toml` bump cannot publish a manifest every installed app would compare against.
- [x] Upload per target the updater archive Tauri wrote (`bundle/macos/mailypoppins.app.tar.gz`) and its `.sig`, renamed to `mailypoppins-desktop-$TARGET.app.tar.gz` and `.app.tar.gz.sig`, replacing the hand-made `tar`; the signature covers the bytes, so the uploaded file must be the signed one, and the asset name and its `.sha256` stay.
- [x] Add a `desktop-manifest` job (`needs: desktop-macos`) that downloads the `.sig` asset and writes `latest.json` with `jq -n --rawfile`, then uploads it:
  `{version, notes, pub_date, platforms: {"darwin-aarch64": {signature, url}}}`, with `url` the tag's `releases/download/vX.Y.Z/mailypoppins-desktop-aarch64-apple-darwin.app.tar.gz` and `signature` the `.sig` file's content.
- [x] Make the release "latest" only after `latest.json` is up.
  Preferred: `create-release` passes `--latest=false` and `desktop-manifest` ends with `gh release edit "$GITHUB_REF_NAME" --latest`, so every installed app keeps reading the previous, complete manifest until the new one is whole.
  A draft release would do the same but breaks `homebrew-tap`, which `curl`s the `.sha256` assets from the public download URL.
  If the desktop build fails, the manifest job is skipped and "latest" stays on the previous release, with no half-filled manifest.

## Sidecar handling

- The bundled `mp` updates with the app, since the plugin swaps the whole `.app`; the app and its daemon binary never drift apart.
- A Homebrew formula or `cargo install` `mp` is not touched and drifts further with each app update.
  The app does not use it while the sidecar exists (the sidecar comes before `PATH` in `resolve_mp_binary`), but a daemon that `mp` starts is refused by the app with the restart screen, and the restart screen's Restart replaces it with the bundled version; the "do not combine" rule of `docs/release-process.md` ("`mp` on PATH from the app") stays the answer, and the symlink route follows the app through every update.
- After a swap, the old daemon goes on running the old binary, which was moved to the plugin's backup location.
- Without the stop in step 5, the new app's first connect meets that daemon, the probe reads the new `mp --version` (the cache key changed with size and modification time), and the user gets the blocking restart screen; its Restart already fixes it.
  Stopping the daemon before the relaunch avoids the screen; Stage 2's `{from, to}` record avoids it for the update applied at quit.
- In the "Later" window the old app runs over the new bundle's files: a reconnect probes the new `mp` and shows the restart screen, and an embedded editor started then reads the new `nvim/` resources.
  Both are recoverable, and the indicator says a restart is pending.
- A daemon run by `mp daemon install-service` has `KeepAlive` with `SuccessfulExit` false (`src/daemon/templates/dev.mailypoppins.daemon.plist`), so a clean `daemon.stop` is not restarted by launchd; through the documented symlink the service path follows the bundle.

## Tests and verification

Rust unit tests, no window and no network:

- The cooldown and skip decision as a pure function of `last_check`, now, `skipped_version` and the manual flag.
- The "should check at all" rule: debug build, fixture mode, not in a bundle.
- `update-state.json` read and write, a missing or broken file reading as empty.
- The `auto_update` key in `settings.rs` (`check` refuses anything but `on` and `off`), with the ts-rs type regenerated.
- The Stage 2 mismatch rule: a daemon of the recorded `from` version restarts quietly, any other version still shows the screen.
- A `latest.json` built by the workflow's `jq` command from a sample `.sig` file parses with the plugin's own manifest type.

Vitest: the sidebar entry, the palette commands, the progress card and the Restart now / Later choice against a mocked command.

On the Mac, with two real tags (a test pair such as `v0.11.0-rc.1` and `-rc.2` would be prereleases and invisible to `releases/latest`, so use a fork or a throwaway repository with its own `endpoints`):

- First install of version N from the DMG downloaded in a browser, with the Gatekeeper step.
- Update from N to N+1 through the palette, the ad-hoc identity on both.
- `xattr -l /Applications/mailypoppins.app` after the update shows no `com.apple.quarantine`, and the relaunch opens without the Privacy & Security step.
- `codesign -dv /Applications/mailypoppins.app` reports an ad-hoc signature before and after.
- With an account on `keyring` and Terminal.app as the editor route, note whether the Keychain or the Automation prompt returns after the update.
- The daemon after Restart now is version N+1 (`mp daemon status` through the bundled binary), and "Later" followed by a quit and a launch gives the restart screen once.
- From a standard (non-admin) account, the admin-password fallback appears and a cancel leaves the old app working.
- A dry run of the workflow on the throwaway repository: the `.app.tar.gz` and `.sig` assets, a valid `latest.json`, and the "latest" flag moved only after it.

## Alternatives considered

- A GitHub-releases check with a banner that opens the release page, no self-replacement.
  No new plugin and no key, but every update repeats the download, the drag and the Gatekeeper step, which is what PERSO-85 wants gone.
  Kept as the failure fallback of step 7.
- The Homebrew cask as the only channel.
  `brew upgrade` replaces the app well, but a cask download is quarantined, so an unsigned app is refused; the template is gated on #0012 for that reason.
  Kept as the second channel in Stage 3, with `auto_updates true`.
- Sparkle.
  Native and mature, with its own EdDSA appcast, but there is no official Tauri integration, so it would be custom Swift around a Rust app, and it prefers a Developer ID build.
  Dropped.

## Open points for Sylvain

- Adopting `tauri-plugin-updater` is a new dependency and needs your permission under the dependency rule in `AGENTS.md`; `tauri-plugin-process` is not needed if the relaunch stays in Rust, as proposed.
- Custody of the minisign private key: the repository secret plus a copy in Proton Pass, password included; losing it ends updates for every installed app.
- The cooldown: 24 h after a successful check is proposed.
- The `auto_update` key name and its `on`/`off` values, against the dotted `updates.auto_install` of the brief.
- Making the release "latest" late (`--latest=false`, then `gh release edit --latest`) changes when the CLI release becomes "latest" too.

## Docs to update when this lands

- [x] `docs/release-process.md`: the key, the secrets, the manifest job, the late "latest", and the update path for a user.
- [x] `clients/desktop/docs/shell.md`: the sidebar entry, the palette commands, the progress card, the Settings lines.
- [x] `clients/desktop/docs/rust-layer.md`: the update commands and `update-state.json`.
- [x] `clients/desktop/README.md`: the update route beside the install and uninstall lines.
- [ ] `docs/tickets/0132-gui-distribution-and-release.md`: a pointer to this ticket.
- [x] `website/src/pages/getting-started.astro`: "Mac app" says later versions arrive in the app.
- [x] `CHANGELOG.md` under `[Unreleased]`.
