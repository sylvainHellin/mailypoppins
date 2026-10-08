# Release process

How a version of mailypoppins gets from `main` to GitHub Releases and the
Homebrew tap. The pipeline lives in
[.github/workflows/release.yml](../.github/workflows/release.yml)
(ticket #0011); the Homebrew pieces in
[packaging/homebrew/](../packaging/homebrew/) and
[scripts/update-homebrew-formula.sh](../scripts/update-homebrew-formula.sh)
(ticket #0013).

## Cutting a release

1. **Bump the version** in `Cargo.toml` (`version = "X.Y.Z"`), then run
   `cargo build` so `Cargo.lock` picks it up. Commit both.
2. **Update `CHANGELOG.md`**: rename the `## [Unreleased]` section to
   `## [X.Y.Z] - YYYY-MM-DD` and add a fresh empty `## [Unreleased]`
   above it. The release workflow extracts the notes for the GitHub
   release from the first `## [X.Y.Z]` section, so the header must match
   the tag version exactly.
3. **Tag and push**:

   ```sh
   git tag vX.Y.Z
   git push origin main vX.Y.Z
   ```

4. The `Release` workflow then, automatically:
   - creates the GitHub release with the changelog section as notes;
   - builds and attaches `mailypoppins-<target>.tar.gz` + `.sha256` for:

     | Target | Runner | Notes |
     |---|---|---|
     | `aarch64-apple-darwin` | macos-latest | |
     | `x86_64-unknown-linux-gnu` | ubuntu-latest | links system OpenSSL |
     | `x86_64-unknown-linux-musl` | ubuntu-latest | fully static, `vendored-openssl` feature |

   - builds the desktop app on macOS and attaches
     `mailypoppins-desktop-<target>.dmg` and
     `mailypoppins-desktop-<target>.app.tar.gz`, each with a `.sha256`, for
     `aarch64-apple-darwin` (the `desktop-macos` job, see
     [The desktop app](#the-desktop-app));
   - renders the Homebrew formula from the release checksums and pushes
     it to the tap repo (skipped with a notice while the
     `TAP_DEPLOY_KEY` secret is absent).

There is no Intel macOS build (`x86_64-apple-darwin`) of the CLI or the
app; an Intel Mac builds `mp` from source with `cargo install --path .`.

Each archive contains the single `mp` binary. The `vendored-openssl`
cargo feature (optional `openssl/vendored` dependency in `Cargo.toml`)
exists only for the musl target, where no system OpenSSL is available; it
is not compiled into default builds.

Plain `cargo test` also runs on every push / PR via
[.github/workflows/ci.yml](../.github/workflows/ci.yml).

## The desktop app

The `desktop-macos` job runs `pnpm bundle --target <target>` in
`clients/desktop` ([scripts/bundle.ts](../clients/desktop/scripts/bundle.ts)), the
same command as a local build:

1. it builds the root crate's `mp` in release for the target;
2. it copies it to `clients/desktop/src-tauri/binaries/mp-<target>`, the
   target-suffixed name Tauri's `bundle.externalBin` wants;
3. it runs `tauri build --target <target>` with
   [tauri.bundle.conf.json](../clients/desktop/src-tauri/tauri.bundle.conf.json),
   which holds the `externalBin` entry, and with the bundle's version set to
   `mp`'s.

The bundler drops the suffix, so the app carries
`mailypoppins.app/Contents/MacOS/mp` beside its own executable, which is the
first place the app looks for the `mp` that starts its daemon (after
`MP_DESKTOP_MP_BIN`). The `externalBin` entry is kept out of
`tauri.conf.json` because tauri-build copies every sidecar on every build and
fails when one is missing, which would make `pnpm tauri dev` and `cargo test`
in `src-tauri` need a staged `mp`.

The app checks the daemon it reaches against that `mp`: a daemon of another
version is refused with the restart screen, whose Restart runs
`mp daemon restart` with the bundled binary
([rust-layer.md](../clients/desktop/docs/rust-layer.md), "Conventions").

The job uses plain `tauri build` rather than `tauri-apps/tauri-action`: the
release already exists with the changelog notes, the sidecar has to be staged
first anyway, the asset names stay the ones the cask template expects, and
Tauri's bundler reads the Apple signing and notarization variables itself.

### Signing and notarization (#0012)

The app and the DMGs are unsigned until
[#0012](tickets/0012-apple-developer-id-signing.md). The workflow step has a
marked, commented-out `env:` block; signing is adding these repository
secrets and uncommenting it:

| Secret | Holds |
|---|---|
| `APPLE_CERTIFICATE` | base64 of the Developer ID Application `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | the `.p12`'s export password |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: <name> (<team id>)` |
| `APPLE_ID` | the Apple account's email, for notarytool |
| `APPLE_PASSWORD` | an app-specific password of that account |
| `APPLE_TEAM_ID` | the 10-character team id |

`tauri build` then imports the certificate into a temporary keychain, signs
the app and the sidecar, notarizes and staples.

### `mp` on PATH from the app

The app's `mp` is a whole CLI. Until the cask exists, the supported way to use
it from a shell is a symlink, which keeps the CLI, the TUI and the daemon the
app starts on one binary of one version:

```sh
ln -s /Applications/mailypoppins.app/Contents/MacOS/mp /usr/local/bin/mp
```

Any directory on `PATH` works (`~/.local/bin`, `/opt/homebrew/bin`). Do not
combine it with the Homebrew formula or a `cargo install`: two `mp` of two
versions is the mismatch the app refuses. `mp daemon install-service` run
through the symlink bakes the symlink's path, which survives an app update.

### Local build

```sh
cd clients/desktop
pnpm install
pnpm bundle                                # builds mp, stages it, tauri build
MP_SIDECAR_BIN=$CARGO_TARGET_DIR/release/mp pnpm bundle   # an existing mp instead
```

The bundle lands in `<target dir>/<target>/release/bundle/`, `macos/` for
the `.app` and `dmg/` for the DMG, where `<target dir>` is
`$CARGO_TARGET_DIR` or `clients/desktop/src-tauri/target`.

## Homebrew tap

Users install with:

```sh
brew tap sylvainHellin/mailypoppins
brew install mailypoppins
```

The formula ([packaging/homebrew/mailypoppins.rb.tmpl](../packaging/homebrew/mailypoppins.rb.tmpl))
downloads prebuilt release binaries instead of building from source, so
the macOS artifact keeps its code signature once
[#0012 apple-developer-id-signing](tickets/0012-apple-developer-id-signing.md)
ships. macOS uses the native darwin builds; Linux uses the static musl
build.

### One-time manual setup (not automatable from this repo)

> **Already provisioned** (2026-07-11). The tap repo, deploy keypair, and
> `TAP_DEPLOY_KEY` secret are all in place; the pipeline goes fully live
> on the next `v*` tag push. The steps below are kept as reference for
> re-provisioning or **rotating** the deploy key.

The tap push authenticates with an **SSH deploy key** (a keypair scoped to
a single repo), not a PAT. The private half lives as an Actions secret on
this repo; the public half is a write-enabled deploy key on the tap repo.

1. Create the public tap repository
   **`sylvainHellin/homebrew-mailypoppins`** on GitHub (empty, default
   branch `main`, with any initial commit so it can be cloned).
2. Generate a dedicated ed25519 keypair (no passphrase, so CI can use it
   non-interactively):

   ```sh
   ssh-keygen -t ed25519 -N '' -C 'mailypoppins-tap-deploy' \
     -f /tmp/mailypoppins-tap-deploy
   ```

3. On **`sylvainHellin/homebrew-mailypoppins`**, add the **public** half
   (`/tmp/mailypoppins-tap-deploy.pub`) as a deploy key
   (Settings -> Deploy keys -> Add deploy key) and **tick "Allow write
   access"** -- the workflow pushes commits through it.
4. On **`sylvainHellin/mailypoppins`**, add the **private** half
   (`/tmp/mailypoppins-tap-deploy`, the whole PEM including the
   `-----BEGIN/END-----` lines) as an Actions secret named
   **`TAP_DEPLOY_KEY`**:

   ```sh
   gh secret set TAP_DEPLOY_KEY --repo sylvainHellin/mailypoppins \
     < /tmp/mailypoppins-tap-deploy
   ```

5. Delete the local key files (`rm /tmp/mailypoppins-tap-deploy*`) once
   both halves are installed.
6. Tag the next release (or re-run the `homebrew-tap` job of an existing
   release run). The job loads `TAP_DEPLOY_KEY` into ssh-agent
   (`webfactory/ssh-agent`), clones the tap over SSH
   (`git@github.com:sylvainHellin/homebrew-mailypoppins.git`), and writes
   `Formula/mailypoppins.rb` to it.

If steps 1-4 are ever undone (e.g. a rotation in progress), the
`homebrew-tap` job skips itself with a notice and the rest of the release
still succeeds.

To render the formula locally for inspection (needs a published release
with checksum assets):

```sh
scripts/update-homebrew-formula.sh 0.9.0
```

### The app's cask (not published yet)

[packaging/homebrew/mailypoppins-app.rb.tmpl](../packaging/homebrew/mailypoppins-app.rb.tmpl)
is a cask skeleton for the desktop app, for `Casks/mailypoppins-app.rb` in
the tap. It installs the app, links its `mp` into Homebrew's `bin`, stops the
daemon before an uninstall, and conflicts with the `mailypoppins` formula. It
is gated on the signed build: a cask download is quarantined, so an unsigned
app would not open. Once #0012 signs the DMGs, add a renderer for its
placeholders beside `scripts/update-homebrew-formula.sh` and push the result
from the `homebrew-tap` job; the template's header lists both steps.

## Installing the daemon at login

A release installs a binary; it does not arrange for a daemon to be running when
the user logs in. That is one command per machine, and the same one on both
supported platforms:

```sh
mp daemon install-service        # writes the unit and enables it
mp daemon install-service --check
mp daemon uninstall-service
```

On Linux it writes `$XDG_CONFIG_HOME/systemd/user/mailypoppins.service` and runs
`systemctl --user daemon-reload` then `systemctl --user enable --now
mailypoppins.service`. On macOS it writes
`~/Library/LaunchAgents/dev.mailypoppins.daemon.plist` and runs `launchctl
bootstrap gui/<uid> <plist>`. Both files run `mp daemon run` in the foreground
by absolute path, and both carry the data and config directories the installing
`mp` resolved. A host with no service manager on `PATH` still gets the file
written and the commands printed to run by hand, and exits 0. The whole surface,
its stdout and its refusals are in
[daemon-operations.md](daemon-operations.md#login-mode).

The macOS half has never been run against a real `launchctl` (see below), so a
first install on a Mac is worth watching rather than scripting.

## After a `cargo install`, restart the daemon

`cargo install --path .` replaces the binary on disk and leaves the **running**
daemon exactly where it was, still executing the code it was started with. Every
client then talks to the old engine while `mp --version` reports the new one,
which is the most confusing state this architecture can be in.

```sh
cargo install --path . && mp daemon restart
```

`mp daemon restart` prints the stop's line and nothing about the start, so
`mp daemon status` is what confirms the replacement is up; its instance id is a
new one. A login-installed service is restarted through the manager that owns the
process instead: `systemctl --user restart mailypoppins.service` on Linux, and
on macOS the launchd equivalent, which is untested here with the rest of the
launchd half.

### Local install from source

On the Mac, [scripts/install-local.sh](../scripts/install-local.sh) does the
whole update in one run: it pulls, runs `cargo install --path . --locked`,
bundles the desktop app with that same `mp` as its sidecar, quits the running
app, swaps the new bundle into `/Applications/mailypoppins.app`, restarts the
daemon once, relaunches the app if it was running, and prints the commit, the
versions and the daemon's state.

```sh
scripts/install-local.sh             # pull, mp, the app, the daemon
scripts/install-local.sh --no-gui    # mp only
scripts/install-local.sh --dry-run   # print what it would run
```

The daemon is not stopped before the install, for the reason above. When the
launchd agent is loaded, the restart is `mp daemon stop` then `launchctl
kickstart gui/<uid>/dev.mailypoppins.daemon`, because `mp daemon restart`
would start the replacement outside launchd, and the agent's `KeepAlive
{SuccessfulExit: false}` leaves a stopped daemon stopped. The script refuses a
tree with uncommitted changes (`--allow-dirty` overrides) and an app installed
by the Homebrew cask; the reasoning behind each step is in its header comment.

## Upgrades and the login-start service

`mp daemon install-service` bakes the absolute path of the installing binary
into `ExecStart` / `ProgramArguments` (see
[daemon-operations.md](daemon-operations.md#login-mode)), so a binary that is
reinstalled **at a different path** leaves a service pointing at the old one.
The repair is `mp daemon install-service --force`, which rewrites the file and
re-enables it.

Installing from source keeps `~/.cargo/bin/mp` and never moves, so nothing has
to be done there. A Homebrew upgrade moves the real binary into a new
version-stamped Cellar directory while `bin/mp` stays a symlink at a stable
path.
The service bakes `std::env::current_exe()` without canonicalising it, and when the first `mp` on `PATH` is the same file (same device and inode), it bakes that `PATH` entry instead, so a Homebrew install gets the stable `bin/mp` even when the OS reports the Cellar path.
An install whose `mp` is not on `PATH` keeps whatever `current_exe()` returned; a user in that case who upgrades should re-run `mp daemon install-service --force` and `mp daemon restart`.

## Unsigned macOS binaries (until #0012)

Release binaries are not yet codesigned/notarized. `brew install` works
(Homebrew does not quarantine curl-downloaded bottles/binaries), but a
manually downloaded archive from the Releases page will trip Gatekeeper;
users can clear it with `xattr -d com.apple.quarantine mp`. The desktop
app's DMG is the same: after dragging the app to `/Applications`, either
allow it once under System Settings > Privacy & Security ("Open Anyway"
after the first blocked launch; Control-click > Open no longer bypasses
Gatekeeper since macOS 15), or clear the whole bundle with
`xattr -dr com.apple.quarantine /Applications/mailypoppins.app`. Signing is
tracked in [#0012](tickets/0012-apple-developer-id-signing.md).
