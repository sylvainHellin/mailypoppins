# The daemon leftovers

Sylvain decided on 2026-10-22 that the daemon follow-ups are built or removed, none parked (`decision-log.md`, "The six daemon leftovers are all built or removed, none parked").
This plan covers five of the six; the sixth, deleting `spikes/ipc-bench`, is not in this batch.
Each item lands as one or more commits on the `daemon-leftovers` branch, in this order, each with a test that fails without it.

## 1. Credential presence: `account.credentials`

`mp config show` reads the secrets backend and the token cache in the client for its `password = **** / (not set)` column and its `token = …` line, the three `src/config_cmd/show.rs` rows of `CLI_ENGINE_RESIDUE`.

- A query `account.credentials {account?}` answers `{accounts: [{account, auth_method, smtp_password, imap_password, token}]}`, every configured account in file order or the one named.
- `smtp_password` and `imap_password` are booleans for a non-Graph account and `null` for a Graph one; `imap_password` is the IMAP key alone, so a client says "falls back to smtp" itself.
- `token` is `valid`, `expired`, `invalid` or `not_cached` for an `oauth2` or `graph` account and `null` for a password one.
- It lives in the `account.*` family because it is a fact about an account, and `config.get` keeps its contract of never probing a secret.
- No value, length or key name travels; the type is `mp_protocol::account::AccountCredentials`, so the desktop decodes it through ts-rs.
- `mp config show` renders from it; the desktop Settings card shows the same two facts beside each server and the token under OAuth2 (the decision's "the desktop Settings can show it too"); the TUI has no settings screen and shows nothing.
- The signature block of `mp config show` moves onto `signature.list` and `signature.read` (item 5), so the command reads nothing locally but paths.

## 2. Scoped CLI flags

`-s/--signature`, `--no-signature` and `-A/--account` are `global = true` on `Cli`, so every subcommand's help lists them, `mp daemon status -A x` is accepted, and `mp -s x list` silently ignores `-s`.

- `-s` and `--no-signature` move onto the four commands that write a signature into a draft: `new`, `reply`, `forward`, and `send`, where they need `--invite`.
- `-A` stays global, because `mp -A proton contacts search` before the subcommand is how it is used, and is refused, and hidden from the help, on the commands it means nothing for: `daemon *`, `completions`, `dump-keys`, `account list`, `config init|show|add-account|reset-secrets|path`.
- The refusal is a clap `ArgumentConflict` naming the command, whichever side of the subcommand `-A` is written on.
- The unused `_signature_content` resolution in the preamble goes, since nothing reads it.
- `tests/cli_help_snapshot.rs` and the website pages that list the flags move with it.

## 3. The CLI wizards on the daemon

`mp config init` and `mp config add-account` (and their Graph variants) dial servers, run the device-code flow, store passwords and write `config.toml` in the client: six `CLI_ENGINE_RESIDUE` rows.

- The prompts stay in the client and every engine step becomes a daemon call, in an order that writes nothing until the user has answered everything, as today.
- A new query `config.probe {account, check, password?}` takes a candidate `[[accounts]]` block (the `AccountDraft` the desktop wizard already sends) and `check` of `smtp`, `imap`, `graph` or `mailboxes`; it answers `{ok: true, mailboxes?}` or `{ok: false, error}`, writes nothing and never echoes the password.
- An OAuth2 or Graph candidate is probed with the token its own sign-in staged under `tokens/candidates/`, so the wizard signs in first.
- A probe never reads a configured account's token: a candidate named like a configured account names its own servers, and is refused while it has no staged sign-in of its own.
- `config.oauth2_login` accepts an optional `candidate` block, used instead of the configured account of that name, so the wizard signs in before the account exists.
- The candidate's token is staged, and `config.init` or `config.add_account` promotes it to the account's cache when it writes the account, so an abandoned wizard leaves a configured account's token untouched.
- `config.init` accepts `replace: true`, which is the wizard's "Overwrite? [y/N]" answered yes.
- The file is written by `config.init` or `config.add_account`, and each password the user typed is then stored by `config.set_password`.
- The TOML builders and their comments leave the client; `account_block` writes the file, and the `[email]` table the wizard wrote is left to its serde defaults, which are the same values.
- The six residue rows go and the boundary test stays strict.
- The desktop wizard gains no probe in this batch; it is a follow-up the report names.

## 4. `mp fetch` retired

`mp fetch` is a one-mailbox server listing with inline filters, a duplicate of `mp search --mailbox`.

- The command, `routed_fetch`, its filters, its parity row `LST-11`, the website page and its help snapshot section go.
- `message.list_server`, which only `mp fetch` called, goes with it, from the dispatcher, the protocol docs and the capability list.
- The CHANGELOG says that `mp search --mailbox <name> --from … --after …` replaces it.

## 5. The five daemon gaps the desktop works around

- `signature.removed {name}`: the watcher publishes it when a signature file it announced is gone, as it publishes `draft.removed` for a draft.
- `signature.read`, `signature.create`, `signature.rename`, `signature.delete` and `signature.set_default` on the daemon, answering what the desktop's commands answer today; the TUI's `cs` overlay, the desktop's Signatures dialog and `mp config show` call them.
- The TUI compose wizard lists, opens and creates the signatures it offers through the same methods (the TUI follow-up below).
- `contact.vcard_draft {account, name, address, display_name}`: the daemon writes the draft, the `.vcf` under `_vcards/` and the attachment, and answers the draft and the `.vcf` path; the TUI's `v` and the desktop's `v` call it.
- `contact.changed {account}`: a lifecycle event the daemon publishes whenever an account's contact index is written (a rebuild, the implicit build of a search, the bump after a sync or a send); the desktop re-reads its list on it instead of after its own rebuild only, and the TUI re-reads its contacts view.
- The implicit build's refusal: `contact.search` answers a `rebuild` object `{saved, contacts, kept}` when its implicit build was refused by the cache guard, and every client says so (the CLI on stderr, the TUI on the status line, the desktop as a notice).
- The desktop docs, the fixture daemon and the parity rows lose their "the daemon does not" sentences.

## 6. The TUI follow-ups (done, 2026-10-22)

The review of this batch named four places where the TUI still did the daemon's work in its own process, all older than the batch; they are moved, and `tests/architecture_boundaries.rs` gates them (`TUI_DIRECT_REACHES`, `TUI_DIRECT_RESIDUE`).

- Credentials: `AccountState` no longer loads `ImapConfig`, `SmtpConfig` or `GraphConfig`, whose loads read a password or an OAuth2 token; whether an account can sync or send is `account.credentials`, asked on the account's open thread, and the Graph check reads the configuration alone.
  `mp` launching the TUI skips the preamble's secret reads too.
- Contacts: the Contacts view and the compose wizard's completion read the index through `contact.search` with an empty query, off the UI thread, refresh on `contact.changed` and say on the status line when the search's implicit build was refused; the rows gained `first_seen` and `last_seen`, which the ranking and the detail pane read.
- Drafts: the compose wizard's new draft is `draft.create`, which now records a named signature in `signature:`; `x` approves through `draft.validate` and `draft.approve`, and `ta` appends through `draft.attach`, as the desktop does.
  The `ce` header rewrite stays client-side in both clients until a daemon method rewrites a draft's headers.
- Signatures: the wizard's listing, editing and creation go through `signature.*`.
- The CLI gained `mp signature list|show|new|edit|rename|delete|default` over the same methods, `mp -A <account>` with no subcommand opens the TUI on that account, and the hidden `--daemon` flag became `mp account list --daemon`, the one command it changed.

## Docs

`docs/daemon-protocol.md` (families, kinds, events, changelog), `docs/parity-matrix.md` (ACC-03, ACC-05, ACC-09, ACC-10, LST-11, CON-*), `docs/architecture.md` where it names the residue, the backlog (the items leave it), `clients/desktop/docs/rust-layer.md` and `shell.md`, and the website pages under `website/src/pages/` that list the commands and flags.
