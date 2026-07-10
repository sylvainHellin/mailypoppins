# Lessons learned

Non-obvious gotchas, regressions, and hard-won fixes that are easy to forget. Append a new entry whenever you spend more than ~30 minutes discovering something that was not obvious from the code.

Format: short imperative title, one-paragraph description, and (when useful) a code reference.

## HTML charset must be injected before saving

Incoming HTML bodies are saved as raw bytes from the server. Browsers default to latin-1 when no charset is declared, breaking umlauts and other non-ASCII characters. Always inject `<meta charset="UTF-8">` before writing to disk -- see `ensure_utf8_charset()` in `parse.rs`.

## Signature placement uses a placeholder, not a trailing append

Reply and forward drafts contain a `{{SIGNATURE}}` placeholder between the reply area and the quoted conversation. `markdown_to_html` replaces it at send time so the signature lands between reply and quote. If the placeholder is removed, the signature falls back to end-of-body. Do not "simplify" by always appending -- it will land below the quoted thread.

## Send account is resolved by `from:` address, not active TUI account

At send time, the draft's `from:` field is matched against each account's `default_from` to select the correct SMTP/IMAP/Graph config. Implemented in `resolve_send_account()` in `tui/helpers.rs`. Draft-creation commands (reply, forward, new) auto-insert the active account's `default_from`, but a user could in principle send a draft authored from a different account, and the right config must be used.

## Sent folder dedup uses Message-ID, never UID

Sent `.md` files store `message_id` in frontmatter. Sync skips uploading emails already present on the server by Message-ID. The Sent directory is also never reconciled -- locally-authored files are the source of truth. UIDs are unstable across SELECTs and unusable for dedup across sessions.

## Reconciliation is INBOX + Archive only

Only INBOX and Archive participate in server-driven reconciliation (move/delete detection). Sent is excluded by design (see above). Drafts and other mailboxes are local-only or fetch-only.

## Quoted display names with commas break naive splitting

`"Doe, Jane" <addr>` historically broke send/validate because `.split(',')` split inside the quoted name. Use `split_addresses()` from `parse.rs` for any address-list parsing. Don't reach for `.split(',')` on email headers.

## STARTTLS requires a fake greeting

`async_imap` expects an IMAP greeting line on connect. Plain TCP STARTTLS connections (e.g. Proton Bridge on port 1143) don't have one until after the upgrade. The `ImapStream` wrapper in `imap_client/mod.rs` injects a fake greeting so `async_imap` can negotiate. Implicit-TLS connections (port 993) bypass this.

## Self-signed certificates need `accept_invalid_certs` per account

Proton Bridge ships with a self-signed cert. Both IMAP and SMTP code paths honour an `accept_invalid_certs` flag per account. Don't disable cert validation globally -- keep it per-account.

## Inline images count as attachments

A MIME part with `Content-Disposition: inline` and a filename (PDF, image, etc.) is still an attachment for our purposes. `is_attachment_part()` in `parse.rs` treats inline non-text parts with a filename as attachments so the paperclip icon and `o`/`O` actions work. Earlier versions only matched explicit `attachment` disposition or inline `image/*`.

## BCC-only emails are valid

Frontmatter validation requires at least one of `to`, `cc`, `bcc` to be non-empty. `to` is `Option<String>` with `#[serde(default)]`. Don't reintroduce a non-optional `to` field.

## Forwarded-attachment paths break when the source email is archived

`create_forward_draft` resolves attachment paths to absolute paths at draft creation time (`src/draft.rs:262-278`). If the source email is then moved from `inbox/` to `archive/`, the hardcoded paths in the draft frontmatter become stale and `send` fails with "Failed to read attachment". Tracked as an open ticket; do not silently break this when refactoring draft creation.

## `cargo install --path .` is required after every change

The user's `email` binary is the installed one, not `target/debug/email`. A code change that builds and tests green is invisible to the running TUI / CLI until you reinstall. This was historically gated behind a codesign script (`./scripts/install.sh`) for keychain ACL stability; the encrypted-file secrets backend removed that need, so plain `cargo install --path .` is canonical again.

## `mailbox_states` must persist to disk, not live only in memory

`AccountState.mailbox_states` (per-role `uid_validity` / `uid_next` /
`exists`) was originally rebuilt every TUI launch, so the first quick
sync fell through to a full Message-ID reconcile (~14 s on a busy IMAP
server). The cache is now written to `<account_dir>/mailbox-states.json`
after every successful Fetch / Sync and reloaded in `AccountState::new`,
making the cold-start sync <2 s. A corrupt or missing file degrades
silently to an empty map -- the worst case is one extra full reconcile,
so we never block startup on cache I/O. Implemented in #0002.

## Adding new files in dotfiles requires a restow

Fish functions, Claude commands/skills, and similar live in `~/dotfiles/<package>/` and are linked into place by GNU Stow. After adding a *new file* to a stow package, run `cd ~/dotfiles && stow -R <package>`. Editing an existing symlinked file is fine -- no restow needed.

## `tokio::runtime::Runtime::new()` panics inside `#[tokio::main]`

Main is `#[tokio::main]`, so a tokio runtime is always live by the time any sync code runs. Calling `tokio::runtime::Runtime::new()` then `rt.block_on(...)` panics with "Cannot start a runtime from within a runtime." Sync sites that need to drive an async future must use `config_cmd::helpers::run_async_blocking`, which detects the existing runtime and spawns a fresh one in a separate OS thread (modelled on `oauth2::load_or_refresh_token_blocking`). The bug went undetected for months because the affected code paths were the wizard's IMAP/SMTP/Graph test calls, which never ran in `cargo test`.

## Attachment paths in drafts must reference the per-account stable mirror

The per-mailbox `<mailbox>/<stem>_attachments/<file>` path follows the email when reconcile detects an inbox -> archive move on the server, so any draft created before the move ends up with stale frontmatter and `email send` fails with `Failed to read attachment`. Each fetched attachment is therefore also hardlinked into `<account>/attachments/<sanitized-message-id>/` (copy fallback on filesystems that disallow hardlinks). `create_forward_draft` writes those stable paths and lazy-hydrates the mirror for emails fetched before the scheme existed. Cleanup happens in `reconcile_local_files` only when the email vanishes from every synced mailbox -- never on dedup or move (the surviving copy still references the same Message-ID). Helpers live in `src/parse.rs`: `sanitize_message_id_for_path`, `stable_attachments_dir`, `link_or_copy`, `account_dir_for_email`. See ticket #0006.

## Display names with non-atext characters break lettre's `Mailbox` parser

Lettre's `Mailbox::from_str` is a strict RFC 5322 parser: a display name must be either an `atom` (atext only) or a `quoted-string`. Real senders routinely violate this -- TUM mailing lists like `CCBE_Researchers [TUBVCMS] <researchers.ccbe@ed.tum.de>` ship unquoted `[`/`]`, and unquoted commas in `Last, First <addr>` are also common. `send::normalize_address_for_smtp` auto-quotes display names whose characters fall outside RFC 5322 atext + FWS + `.`, escaping inner `\` and `"` per the quoted-string rules. Bare addresses, atext-only names, and already-quoted names pass through unchanged. Tests live in `src/send.rs` under `normalize_*`.

`send_email` parses each address **twice** for different purposes and both sites must use the normalizer:

1. Once per role to build the message *headers* via `Message::builder().to(mbox)/.cc(mbox)/.bcc(mbox)`, plus `from` and `reply_to`.
2. Once per recipient inside the per-recipient `RCPT TO` loop to extract `mbox.email` for the SMTP `Envelope`.

`validate_draft` does the same parse a third time for pre-flight checks. Forgetting any one of them produces a confusing failure mode: validation passes, headers build cleanly, but the offending recipient silently fails at envelope time and the TUI surfaces only `Partial: N/M succeeded` -- the actual `Invalid address '...': Invalid input` line lives in the daily log under `<data_dir>/logs/`. The first fix only patched validation + header build; the envelope loop was caught by a follow-up bug report. Regression test: `normalize_extracts_email_via_mailbox_for_envelope`.

## CSP for saved HTML must allow `file:` images because `cid:` is rewritten at save time

Saved `.html` companions get a `<meta http-equiv="Content-Security-Policy">` tag injected at save time (`inject_csp_meta` in `src/parse.rs`) to block scripts and remote tracking pixels when the file is opened in a browser. The obvious policy `img-src data: cid:` silently breaks inline images: `rewrite_cid_references` runs *before* the file is written and replaces every `cid:` URL with an absolute `file://` path to the extracted attachment, so by the time the browser evaluates the CSP there are no `cid:` URLs left. The policy therefore includes `file:` in `img-src`. This is safe because `script-src 'none'; connect-src 'none'` leaves no exfiltration channel -- a hostile email can at most *display* a local file it already knows the path of, not send it anywhere.
