---
id: 0135
title: Run a command when a matching message arrives (mail hooks)
type: feature
priority: now
status: done
created: 2026-09-30
---

## Problem

Nothing in mp could react to a message arriving.
The first user is `pi-remote` on the home server: a mail Sylvain forwards or writes to the assistant account starts a pi session in Paseo, so the action runs with his full rights and the one thing in front of it is the check that the mail really came from him.
The design is `~/dotfiles/pi/.config/pi/.agents/research/2026-09-29-pi-remote-design.md`, Part 1; hooks are generic, so a later deterministic action (a cleanup on a given subject) is the same configuration with another command.

## Fix

### Configuration

`[[accounts.hooks]]` in `config.toml`, per account because the runner is per account runtime (the design's `account` field is the table the entry sits in).
`name`, `mailbox` (a role or a configured server name, default `inbox`), `exec` (argv, no shell), `timeout_secs` (default 60), and a `match` table: `authenticated_from` with `authserv_id`, `to`, `subject` (regex), `headers` (name to regex).
`config::validate_hooks` refuses at load and at reload: a missing or unsafe name, a duplicate, an empty `exec`, a zero timeout, a regex that does not compile, `authenticated_from` without `authserv_id`, and a `match` with no criterion at all.
The hooks are part of `config.get`'s effective account, so editing one restarts that account's runtime.

### The sender check (`src/daemon/hooks/auth.rs`)

`From:` appears once and carries one address, the address is on the list, and the **topmost** `Authentication-Results` header carries `authserv_id` and records a DKIM pass (`header.d`, else the domain of `header.i`) or an SPF pass (`smtp.mailfrom`) for exactly the `From:` domain.
The receiving server prepends its header above everything the message arrived with, so a forged one is never the topmost; a header from another hop, or one with no authserv-id (Exchange Online), is not trusted.
Exact equality rather than DMARC's relaxed alignment, so a subdomain's server cannot vouch for the parent domain.
RFC 8601 parsing strips nested comments outside quoted strings, splits on `;` outside quotes, tolerates `/version` and spaces around `=`.

### The cursor (`src/daemon/hooks/state.rs`, `mod.rs`)

`<account_dir>/hooks-state.json`, outside the store, because the store is a cache a schema bump rebuilds.
Per hook: mailbox key, UIDVALIDITY, `last_uid`, the UIDs above it already considered, the last 512 fired Message-IDs, `armed_at`, `last_run`.
First sight (or a changed mailbox) arms the hook at `max(sync_cursors.last_uid, MAX(uid))`; no cursor row yet means no arming yet.
A changed UIDVALIDITY re-arms at the new top and logs that the window is skipped.
`advance` never passes the store's arrival mark, so a late lower UID is still considered.
A hook removed from the configuration loses its cursor (also for an account left with no hooks), so re-adding it never backfills.
The claim is saved before the command runs: at most once per message; `mp hooks replay` is the manual retry.

### The runner and the command

`src/daemon/runtime/hook_runner.rs`, spawned in `config::start_account` beside the scheduler for a ready IMAP runtime with hooks, woken by `completed_ticks` and once at start, bound to the runtime like the scheduler.
Claims are scanned under a process-wide mutex, materialised with `parse::materialisation_dir` (`message.eml`, `message.md`, `attachments/`, `message.json`), then run one at a time (`src/daemon/hooks/exec.rs`): argv, JSON on stdin, `MP_HOOK_*` variables, cwd `$HOME`, killed on timeout, stdout and stderr tails kept, the directory removed after.
Every scan and run logs a `[hooks]` line.

### Methods and CLI

`hook.list` and `hook.test` (queries), `hook.replay` (durable operation), in `src/daemon/methods/hook.rs`.
`mp hooks list [--json]`, `mp hooks test <hook> <selector> [--mailbox] [--json]`, `mp hooks replay <hook> <selector> [--mailbox]`; the account is `-A` or the one account configuring that hook name.

## Tests

- `src/daemon/hooks/auth.rs`: a genuine Gmail stamp; a spoofed `From:` with a failing stamp; a forged pass below the real fail; no stamp; a stamp from another hop; Exchange Online's id-less stamp; a pass for another domain; a subdomain signature for the parent domain; an SPF-only pass; an authenticated sender not on the list; two `From:` headers and two addresses; recipient (plus address), subject and header criteria; the RFC 8601 parser.
- `src/daemon/hooks/mod.rs`: no arming before a sync and no backfill; a spoofed sender is considered and never claimed; the arrival mark holds the cursor for a late lower UID; a message moved out and back does not fire twice; a renumbered mailbox re-arms; a removed hook forgets its cursor; a claimed message runs with its payload and the run is recorded; `prepare` refuses a message `match` rejects.
- `src/daemon/hooks/exec.rs` and `state.rs`: stdin and env, exit codes, timeout kill, a missing program, output tails; cursor advance, fired-id bound, atomic 0600 state file.
- `crates/mp-core/src/config.rs`, `src/daemon/config.rs`: defaults, every refusal, a changed hook updates the account.
- `tests/daemon_hooks.rs`: the three methods over a socket and `mp hooks test` / `list` through the binary.

## Left open

- Graph accounts get no runner: their rows carry no raw message to authenticate.
- Commands of one account run sequentially; a slow hook delays the next.
- A daemon that dies between the claim and the command loses that run (by design; replay it).
