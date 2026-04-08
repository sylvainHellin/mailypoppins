# Contributing

This is a small personal project, but the build flow is documented here so
anyone (including future-me on a fresh Mac) can reproduce it.

## Building from source

Standard Rust install via `cargo`:

```
cargo install --path .
```

On macOS, prefer the wrapper script:

```
./scripts/install.sh
```

It runs `cargo install --path .` and then re-signs the resulting binary with
a stable code-signing identity so the Keychain stops re-prompting on every
rebuild (see next section). The wrapper forwards extra args to cargo, so
`./scripts/install.sh --locked` works too.

On Linux and Windows the wrapper is equivalent to plain `cargo install` --
the codesign step is a no-op.

## macOS Keychain setup (one-time)

### Why

`cargo install` on Apple Silicon produces an ad-hoc signed binary whose
Designated Requirement is `cdhash H"..."` -- a hash of the exact bytes of
that one build. The Keychain ACL stores that hash in the trusted-app list
when you click "Always Allow". Rebuild the binary, and the hash changes, the
ACL no longer matches, and you get re-prompted for every keychain item the
TUI reads at startup (one per `smtp-password-*` / `imap-password-*`).

The fix is to re-sign the installed binary with a stable self-signed
code-signing certificate. The Designated Requirement then becomes something
like `identifier email and certificate leaf = H"<hex>"` -- a constraint on
the exact signing cert, which stays the same across rebuilds as long as the
same cert is used. The Keychain ACL keeps matching and the prompts stay
gone.

(For self-signed dev certs, codesign bakes a cert-hash constraint into the
DR rather than a `certificate leaf[subject.CN] = "email-dev"`-style
constraint. Either form is stable across rebuilds; the hash form is what
you'll actually see.)

### Create the cert

Done once per Mac, by hand, via Keychain Access:

1. Open **Keychain Access** (`/System/Applications/Utilities/Keychain Access.app`).
2. Menu: **Keychain Access > Certificate Assistant > Create a Certificate...**
3. Settings:
   - **Name**: `email-dev`
   - **Identity Type**: `Self Signed Root`
   - **Certificate Type**: `Code Signing`
   - Check **"Let me override defaults"** and set the validity period to
     `3650` days (10 years). Click through the remaining defaults.
4. Click **Create**, then **Done**. The cert lands in the `login` keychain.

**Login, not System.** Certificate Assistant's default is the login keychain,
and that's the right place. Login-keychain private keys auto-unlock on user
login (so `codesign` can use them without extra prompts), creating a cert
there needs no admin rights, and the cert is scoped to your user account
rather than shared with every user on the machine. The System keychain
would require sudo, need a separately-unlocked `securityd`, and expose the
private key to every user on the Mac -- all downsides with no upside for a
single-user dev cert.

The cert does **not** need to be marked "Always Trust", and you do **not**
need to set a trust override in the Trust section of the cert's Get Info
panel. We are not trying to satisfy Gatekeeper, and we are not relying on
`security find-identity -v` (which filters out untrusted roots). `codesign
--sign` happily uses an untrusted self-signed identity as long as it has a
matching private key and the Code Signing EKU -- both of which Certificate
Assistant sets automatically when you pick "Code Signing" as the type.

### Verify the identity is detected

```
security find-identity -p codesigning
```

Note the **missing `-v` flag** -- that's deliberate. Expected output:

```
Policy: Code Signing
  Matching identities
  1) <HEX> "email-dev" (CSSMERR_TP_NOT_TRUSTED)
     1 identities found
```

The `CSSMERR_TP_NOT_TRUSTED` tag is expected and harmless: it means the cert
is not trust-anchored for the code-signing policy, which is why `-v` would
filter it out. `codesign` still uses it fine. If you run
`find-identity -v -p codesigning` and see `0 valid identities found`, that's
also expected -- don't be alarmed.

If `find-identity -p codesigning` (without `-v`) shows nothing at all, then
either the cert is missing the Code Signing EKU (redo Certificate Assistant
and make sure "Certificate Type" is set to "Code Signing"), or the cert and
private key landed in a different keychain. Log out and back in if needed.

### Install and verify

```
./scripts/install.sh
```

The first time you run this, the Keychain will prompt once: "codesign wants
to sign using key email-dev in your keychain." Click **Always Allow**. This
adds `/usr/bin/codesign` to the `email-dev` private key's ACL permanently,
so subsequent rebuilds sign silently. (If you click just "Allow", you'll be
prompted again on every rebuild -- fix it by clicking Always Allow next
time, or by editing the private key's Access Control in Keychain Access.)

You should see something like:

```
codesign-macos: signed /Users/<you>/.cargo/bin/email
  designated => identifier email and certificate leaf = H"<hex>"
```

You can also check the signature directly:

```
codesign -d -r - ~/.cargo/bin/email
```

Look for `designated => identifier email and certificate leaf = H"..."`.
If you still see `designated => cdhash H"..."`, the script did not run or
used the wrong identity name -- check `EMAIL_CODESIGN_IDENTITY`.

### First-run after the migration

After `./scripts/install.sh` succeeds, launch the TUI:

```
email tui
```

The Keychain will prompt three times -- once per item the TUI reads
(`smtp-password-*`, `imap-password-*`). Click **Always Allow** on each.
These ACLs still trust the old cdhash DR but not the new cert-hash one, so
this is the final round of prompts. From then on, every subsequent rebuild
+ relaunch should be silent.

If it isn't silent, run `codesign -d -r -` again and confirm the DR is
still `identifier email and certificate leaf = H"..."`. If it is, the issue
is that a Keychain prompt was dismissed with plain "Allow" instead of
"Always Allow" -- relaunch and click the right button.

### Optional: clean up stale ACL entries

The old `cdhash` entries from previous builds remain in each keychain item's
trusted-app list as harmless cruft. To remove them:

1. Open Keychain Access and search for `email-cli` (or whichever service
   namespace your accounts use, e.g. `smtp-password-tum`).
2. Double-click an item to open it, switch to the **Access Control** tab.
3. Select stale entries from the list and click the `-` button.
4. Save changes (you'll be prompted for your login password).

Most people just leave them.

## Linux / Windows

No setup is needed. `./scripts/install.sh` works the same as
`cargo install --path .` because the codesign step is a no-op outside macOS.

These platforms don't have the rebuild-prompt problem in the first place:
the Linux Secret Service ACL is per session, and Windows DPAPI is keyed to
the user login -- neither cares about the binary's code identity, so a
fresh build is treated like the previous one.

## Troubleshooting

- **`security find-identity -v -p codesigning` reports `0 valid identities`.**
  Expected. Drop the `-v` flag: `security find-identity -p codesigning`.
  Self-signed roots are `CSSMERR_TP_NOT_TRUSTED` for the codeSign policy,
  which `-v` filters out, but `codesign` doesn't care.
- **`security find-identity -p codesigning` (no `-v`) shows nothing.** The
  cert is missing the Code Signing EKU, or the cert/private key pair isn't
  in the login keychain. Redo Certificate Assistant with Certificate Type
  set to "Code Signing". Log out and back in if the cert was just created.
- **Every build prompts for the private key password.** You clicked "Allow"
  instead of "Always Allow" on the codesign prompt. Open Keychain Access,
  find the `email-dev` **private key** (not the cert), Get Info, Access
  Control tab, and either add `/usr/bin/codesign` to the always-allow list
  or tick "Allow all applications to access this item".
- **Script still shows `designated => cdhash`.** The re-sign didn't run, or
  ran with the wrong identity. Check `EMAIL_CODESIGN_IDENTITY` and the
  output of `./scripts/codesign-macos.sh`.
- **Keychain still re-prompts after the migration.** The DR may now be
  stable but the smtp/imap password items don't trust it yet. Click "Always
  Allow" once on each prompt -- subsequent rebuilds should stay silent.
- **Cert is about to expire (10 years from now).** Recreate the cert
  following the steps above. The new cert will have a different hash, so
  the DR changes, and the Keychain will re-prompt once per item one final
  time.

## Tests

```
cargo test
```

All tests run offline in under a second. See `AGENTS.md` for the testing
conventions.
