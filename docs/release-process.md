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
     `HOMEBREW_TAP_TOKEN` secret is absent).

Each archive contains the single `email` binary. The `vendored-openssl`
cargo feature (optional `openssl/vendored` dependency in `Cargo.toml`)
exists only for the musl target, where no system OpenSSL is available; it
is not compiled into default builds.

Plain `cargo test` also runs on every push / PR via
[.github/workflows/ci.yml](../.github/workflows/ci.yml).

## Homebrew tap

Users install with:

```sh
brew install sylvainhellin/email/mailypoppins
```

The formula ([packaging/homebrew/mailypoppins.rb.tmpl](../packaging/homebrew/mailypoppins.rb.tmpl))
downloads prebuilt release binaries instead of building from source, so
the macOS artifact keeps its code signature once
[#0012 apple-developer-id-signing](tickets/0012-apple-developer-id-signing.md)
ships. macOS uses the native darwin builds; Linux uses the static musl
build.

### One-time manual setup (not automatable from this repo)

1. Create the public tap repository
   **`sylvainHellin/homebrew-email`** on GitHub (empty, default branch
   `main`, with any initial commit so it can be cloned).
2. Create a fine-grained PAT with **Contents: read and write** scoped to
   `sylvainHellin/homebrew-email` only.
3. Add it as an Actions secret named **`HOMEBREW_TAP_TOKEN`** on
   `sylvainHellin/mailypoppins`
   (`gh secret set HOMEBREW_TAP_TOKEN --repo sylvainHellin/mailypoppins`).
4. Tag the next release (or re-run the `homebrew-tap` job of an existing
   release run). The job writes `Formula/mailypoppins.rb` to the tap.

Until steps 1-3 are done, the `homebrew-tap` job skips itself with a
notice and the rest of the release still succeeds.

To render the formula locally for inspection (needs a published release
with checksum assets):

```sh
scripts/update-homebrew-formula.sh 0.9.0
```

## Unsigned macOS binaries (until #0012)

Release binaries are not yet codesigned/notarized. `brew install` works
(Homebrew does not quarantine curl-downloaded bottles/binaries), but a
manually downloaded archive from the Releases page will trip Gatekeeper;
users can clear it with `xattr -d com.apple.quarantine email`. Signing is
tracked in [#0012](tickets/0012-apple-developer-id-signing.md).
