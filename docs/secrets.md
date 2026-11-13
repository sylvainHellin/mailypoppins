# Secrets storage

mailypoppins stores SMTP/IMAP passwords and Graph token caches encrypted at
rest with a key derived from the host machine ID and the current user's UID.
There is no prompt at runtime, no GPG dependency, and no Keychain ACL to
break across rebuilds.

## Threat model

What this protects against:

- **Backup leakage.** A Time Machine snapshot, an iCloud Drive sync, a
  Dropbox folder, an `rsync` to a NAS, or an accidental `git add ~/.config`
  cannot be decrypted on any machine other than the one that wrote it. The
  ciphertext is a sealed bag of bytes anywhere else.
- **Cross-machine `cp -a`.** Same property: copying the secrets file to
  another machine yields a file that no longer decrypts.
- **Casual local snooping.** `cat secrets.enc` shows ciphertext, not a
  TOML map of plaintext passwords.

What this does **not** protect against:

- An attacker running as the same user on the same machine. Such an
  attacker can attach a debugger to the running TUI process and dump the
  in-memory passwords, drive `Mail.app` via AppleScript, read
  `~/Library/Mail/`, exfiltrate browser cookies, etc. There is no defense
  against same-user execution short of biometric-gated Secure Enclave keys
  (which we deliberately reject for UX reasons).
- A malicious wholesale clone of the disk image including the TPM /
  IOPlatformUUID hardware (e.g. by an evil-maid attack with physical
  access). This is well outside the threat model for a personal CLI.

In short: the encrypted file is roughly `chmod 600 secrets.toml` against a
local attacker, and strictly stronger against any kind of off-machine
leakage. The UX and operational properties are dramatically better than
either Keychain (rebuild churn) or `pass` (GPG dependency).

## Key derivation

```
machine_id = <Linux: /etc/machine-id> | <macOS: IOPlatformUUID via ioreg>
ikm        = machine_id || u32_le(getuid()) || b"mailypoppins-v1"
key        = HKDF-SHA256(ikm, salt = b"mailypoppins-secrets", info = b"")[..32]
```

The `machine-uid` Rust crate handles the platform-specific machine-ID
read. WSL behaves like Linux (uses `/etc/machine-id`).

## File layout

```
offset  size  contents
------  ----  --------
0       5     magic    = b"MPSEC"
5       1     version  = 0x01
6       12    nonce    (CSPRNG-generated per write)
18      ..    ciphertext_with_tag (ChaCha20-Poly1305)
```

The plaintext is a TOML document of the form:

```toml
[entries]
"smtp-password-tum"      = "..."
"imap-password-tum"      = "..."
"smtp-password-personal" = "..."
```

The same crypto helpers (`secrets::encrypt_blob` / `secrets::decrypt_blob`)
are reused by `oauth2.rs` to encrypt Graph token caches at
`<mailypoppins_data_dir>/tokens/<account>.enc` (raw blob format -- the plaintext is
JSON, not TOML, but the wrapping is identical).

## What the errors say

Each error a user can meet between `mp config init` and a login names what happened and the command that fixes it (PERSO-199, pinned by `tests/secrets_smoke.rs` and the unit tests in `crates/mp-core/src/secrets.rs`):

```
No SMTP password is stored for account 'tum'. Run `mp config set-password smtp --account tum`.
No IMAP password is stored for account 'tum', nor an SMTP password to fall back to. Run `mp config set-password imap --account tum`.
No SMTP password is stored for account 'tum' in the system keyring (service 'mailypoppins'). Run `mp config set-password smtp --account tum`.
Could not read the SMTP password of account 'tum' from the system keyring: <the keyring's reason>. Unlock the system keyring (on macOS, the login Keychain) and retry, or set `secrets_backend = "encrypted-file"` in /home/me/.config/mailypoppins/config.toml and restart the daemon (`mp daemon restart`), then store the password again.
Could not read this machine's ID, from which the encrypted secrets file's key is derived (<reason>). To store passwords, create /etc/machine-id (`sudo systemd-machine-id-setup`), or set `secrets_backend = "keyring"` in /home/me/.config/mailypoppins/config.toml and restart the daemon (`mp daemon restart`).
```

On a named profile every command carries `--profile <name>`, the configuration named is the profile's (`~/.config/mailypoppins/profiles/<name>/config.toml`), and the keyring service is `mailypoppins-<name>`.
The `/etc/machine-id` advice is printed on Linux only.
With no configuration at all, the first line a command prints says to run `mp config init`, and no password is mentioned.

## Recovery

If the secrets file becomes undecryptable (you restored from a backup onto
a new machine, the kernel handed out a new machine-id, or the file got
corrupted), every command that needs a password will fail with:

```
Cannot decrypt secrets store at <path> (<reason>). This usually means the file was created on a different machine or by a different user. Run `mp config reset-secrets` to wipe and re-enter passwords.
```

`mp config reset-secrets`, which that refusal does not stop:

1. Confirms the destructive operation interactively.
2. Removes `~/.config/mailypoppins/secrets.enc`.
3. Removes all `*.enc` files under `<mailypoppins_data_dir>/tokens/`.
4. Walks each configured `[[accounts]]` and re-prompts for SMTP / IMAP
   passwords. For Graph accounts, prints a hint to re-run
   `mp config oauth2-login --account <name>`.

## Opting into the OS keyring

Power users can switch to the OS keyring by adding to
`~/.config/mailypoppins/config.toml`:

```toml
secrets_backend = "keyring"
```

This swaps the storage at startup. There is no automatic migration: you
will need to re-set passwords via `mp config set-password <which>
--account <name>` after flipping the switch.
A running daemon that has already opened its backend refuses a `config.reload` that flips `secrets_backend` (`-32007`, "restart the daemon to apply"), so run `mp daemon restart` after the edit.

The keyring service name is `mailypoppins`, and `mailypoppins-<name>` on a named profile (`mp --profile <name>`), so a profile keeps its passwords apart from the default profile's even for an account of the same name.
It was `email-cli` before #0022, so on the default profile a lookup that misses under `mailypoppins` retries under `email-cli` and nobody who opted in before the rename loses access to a stored password.
A profile reads, writes and deletes under its own service alone, never under `mailypoppins` or `email-cli`.
Writes and deletes go to `mailypoppins` only, so the next `mp config set-password` moves the credential across and leaves a stale, harmless `email-cli` entry behind.
Clear those by hand if you want them gone (Keychain Access on macOS, `keyctl` from keyutils on Linux).

The OS keyring is the login Keychain on macOS and the kernel keyring (keyutils, the `keyring` crate's `linux-native` store) on Linux and WSL, not the Secret Service: it needs no desktop session or D-Bus, which is why `tests/secrets_smoke.rs` runs the keyring backend headless on Linux.
The kernel keyring lives in memory: a reboot clears it, and after a logout it lasts only as long as the user's persistent keyring (a few days by default, `/proc/sys/kernel/keys/persistent_keyring_expiry`).
On Linux, a password stored in the keyring has to be set again after every reboot, so the encrypted file is the better choice there for anything but a throwaway setup.

On macOS, the keyring backend will re-prompt for every keychain item every
time the binary is rebuilt with a different code-signing identity (which
`cargo install` produces by default). It is a fine choice for end users
installing a stably-signed Homebrew artifact, and a poor choice for active
local development. The encrypted file backend is the right default until
release signing is set up.
