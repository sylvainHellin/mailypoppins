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
     | `x86_64-apple-darwin` | macos-latest | cross-compiled |
     | `x86_64-unknown-linux-gnu` | ubuntu-latest | links system OpenSSL |
     | `x86_64-unknown-linux-musl` | ubuntu-latest | fully static, `vendored-openssl` feature |

   - renders the Homebrew formula from the release checksums and pushes
     it to the tap repo (skipped with a notice while the
     `TAP_DEPLOY_KEY` secret is absent).

Each archive contains the single `mp` binary. The `vendored-openssl`
cargo feature (optional `openssl/vendored` dependency in `Cargo.toml`)
exists only for the musl target, where no system OpenSSL is available; it
is not compiled into default builds.

Plain `cargo test` also runs on every push / PR via
[.github/workflows/ci.yml](../.github/workflows/ci.yml).

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

## Unsigned macOS binaries (until #0012)

Release binaries are not yet codesigned/notarized. `brew install` works
(Homebrew does not quarantine curl-downloaded bottles/binaries), but a
manually downloaded archive from the Releases page will trip Gatekeeper;
users can clear it with `xattr -d com.apple.quarantine mp`. Signing is
tracked in [#0012](tickets/0012-apple-developer-id-signing.md).
