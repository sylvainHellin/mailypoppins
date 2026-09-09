# Daemon dependency due diligence

Unit P1a-U7 of `.agents/workflow/native-gui-daemon/plan.md`, ticket #0119.
Section 2 of that plan lists the crates the daemon phases might need and takes a position on each.
This file is the written due diligence behind those positions: per crate the version, the licence, the maintenance signal, what it adds to the lock file, whether it builds on macOS and Linux, and the verdict.

Facts were taken from the crates.io API on 2026-09-09, not from memory, and checked against `Cargo.lock` (lockfile v4, 460 packages) at `d8294db` with rustc / cargo 1.96.0.
Cached availability was read from `~/.cargo/registry/cache/`, which matters only because a cached crate can be adopted with no network.

The short answer has not moved since the plan was written.
One promotion is needed and it downloads nothing; every new crate on the candidate list is declined for phases 0 through 3b; `schemars` reopens at Phase 7 and nothing else reopens on a date.

## Already direct dependencies

These are in `Cargo.toml` today, so the daemon adds no dependency by using them.
The versions below are what `Cargo.lock` resolves; the latest published version is given where it differs, since a lock bump is a separate decision from an adoption.

| Crate | Locked | Latest | Licence | Last release | What the daemon uses it for |
| --- | --- | --- | --- | --- | --- |
| `tokio` (features `full`) | 1.49.0 | 1.53.1 | MIT | 2026-07-20 | Runtime, `UnixListener` / `UnixStream`, `signal`, `process`, `sync`, `spawn_blocking` |
| `serde_json` | 1.0.149 | 1.0.151 | MIT OR Apache-2.0 | 2026-07-20 | NDJSON framing, wire types, `raw_value` for splice-without-re-encode |
| `libc` | 0.2.180 | 0.2.189 | MIT OR Apache-2.0 | 2026-07-21 | `flock` on `daemon.start.lock`, `fchmod` / `umask` for the 0600 / 0700 runtime paths |
| `chrono` | 0.4.45 | 0.4.45 | MIT OR Apache-2.0 | 2026-06-04 | `started_at`, hold countdowns, handle `expires_at` |
| `tempfile` (dev) | 3.24.0 | 3.27.0 | MIT OR Apache-2.0 | 2026-03-11 | Temp data and config roots in every daemon test |
| `insta` (dev) | 1.47.2 | 1.48.0 | Apache-2.0 | 2026-06-11 | Optional snapshot form of the Phase 2 protocol fixtures |

All six are maintained on a months-not-years cadence and all six are platform-independent or already
exercised on both targets by the current build.
`libc` has published a `1.0.0-alpha.4`; the 0.2 train is the one to stay on until that stabilises.

### `UnixStream::peer_cred` is available on both targets

The plan's claim that peer credentials need no new crate holds, verified in the vendored source rather than from documentation.

- `tokio-1.49.0/src/net/unix/stream.rs:939` defines `pub fn peer_cred(&self) -> io::Result<UCred>`, gated on the `net` feature, which `full` includes.
- `tokio-1.49.0/src/net/unix/ucred.rs` selects `impl_linux::get_peer_cred` for `linux` (and `redox`, `android`, `openbsd`, `haiku`, `cygwin`) and `impl_macos::get_peer_cred` for `macos` (and `ios`, `tvos`, `watchos`, `visionos`).
- The macOS implementation calls `getsockopt(SOL_LOCAL, LOCAL_PEEREPID)` for the pid and `getpeereid` for uid and gid, so the pid is present there too, not just on Linux.

So the daemon can verify uid and gid on both targets and log the peer pid, with zero new dependencies.

## Promotion candidates, already in the lock file

Promoting one of these to a direct dependency changes `Cargo.toml` and, for `thiserror`, nothing in `Cargo.lock`.
The repo has the precedent twice (`libc`, `percent-encoding`), each with a comment in `Cargo.toml` saying why, and a promotion should follow it.

### `thiserror`

- Versions: `1.0.69` and `2.0.17` are both in the lock, pulled in by `async-imap`, `dialoguer` and `html2text`; `2.0.20` is the latest published (2026-08-08). `1.0.69` was published 2024-11-10 and is the frozen end of the 1 train.
- Licence: MIT OR Apache-2.0 on both trains.
- Maintenance: dtolnay, 351 M recent downloads, `dtolnay/thiserror`, releasing regularly on the 2 train.
- Transitive additions: none. `thiserror-impl` at the matching version is already in the lock for both trains, and both are in the local cache (`thiserror-1.0.69`, `thiserror-2.0.17` through `2.0.20`), so an offline build resolves either.
- Platform support: proc macro plus `core::fmt`, no platform-conditional code, no `unsafe`.
- MSRV: 1.61 for the 1 train, 1.71 for the 2 train, both below the 1.71 that `tokio` already forces.
- Verdict: promote, for the typed protocol errors of `mp-protocol`, at the point Phase 2 creates that crate. `anyhow` stays where it is for internal plumbing; it cannot carry a stable numeric code across a crate boundary cleanly.
- On which train: the plan names `1.0.69`. Both are download-free, so the argument for 1.x is only that it is what the plan wrote down, while 2.x is the train that still receives fixes and is what a new crate should be written against. This file recommends `thiserror = "2"` on `mp-protocol` and flags the deviation for the Phase 2 dispatcher to confirm; either choice is free and neither blocks P1a.

### `uuid`

- Version: `1.23.4` in the lock, via `icalendar`. MIT OR Apache-2.0.
- Verdict: skip, as the plan says. `rand` (already direct) plus a hex format gives an opaque `instance_id` and opaque operation ids, and nothing in the protocol needs the RFC 4122 shape. Reopens only if a reviewer asks for that shape in writing.

## New crates, none adopted

Each of these would be a first appearance in `Cargo.lock`.
The transitive counts below are first-level dependencies that the lock does not already carry.

### `schemars`

- Version 1.2.2, published 2026-07-27. Licence MIT. MSRV 1.74, edition 2021.
- Maintenance: `GREsau/schemars`, 160 M recent downloads, current.
- Transitive additions: `dyn-clone`, `ref-cast` (plus `ref-cast-impl`) always, and with the default `derive` feature `schemars_derive` and `serde_derive_internals`. None of the five is in the lock today. `serde`, `serde_json`, `syn`, `quote` and `proc-macro2` already are.
- Platform support: pure Rust, no platform-conditional code, both targets fine.
- Cached locally: no, so adoption needs a download.
- Verdict: no, for now. The first consumer is the Phase 7 TypeScript generator; Phase 2 pins the protocol with checked-in request, response, error and event JSON fixtures, which is the load-bearing half of "one definition". If Sylvain reopens it, the shape is `schemars = "1"` on `mp-protocol` only, behind an optional feature, so the `mp` binary never compiles it.

### `notify`

- Versions: 8.2.0 is the current stable, published 2025-08-03, MSRV 1.77. The 9.0 line is at `9.0.0-rc.5` (2026-08-30, edition 2024, MSRV 1.88) and is still a release candidate.
- Licence: CC0-1.0, a public-domain dedication rather than the usual MIT/Apache pair. Nothing here forbids it, but it is worth naming, because CC0 is the one licence on this page that some corporate policies flag.
- Maintenance: `notify-rs/notify`, 37 M recent downloads, active (an rc four weeks ago).
- Transitive additions: `notify-types` and, on Linux, `inotify` (plus `inotify-sys`); on macOS `fsevent-sys` or `kqueue` depending on which backend feature is selected. `libc`, `log`, `walkdir`, `mio` and `bitflags` are already in the lock.
- Platform support: both targets, but with different backends, which is the point of the crate and also the reason its behaviour has to be tested twice.
- Cached locally: no.
- Verdict: no. The plan defers it explicitly and `src/tui/mod.rs:35` already defers it in the product. Phase 3b's draft, signature and config watcher moves the existing 1-second fingerprint poll into the daemon. Revisit only if that poll proves inadequate under test, which is a measurement, not a preference.

### `tokio-util`

- Version 0.7.19, published 2026-07-21. Licence MIT. Same repo and cadence as `tokio`.
- Transitive additions: none. `bytes`, `futures-core`, `futures-sink`, `pin-project-lite` and `tokio` are all already in the lock, so this is the cheapest candidate on the list, and `tokio-util-0.7.19` is in the local cache.
- Platform support: both targets.
- Verdict: no, despite the low cost. The framing requirement is one JSON value per line, which `tokio::io::BufReader::lines()` plus `serde_json` answers in one function that a reader can check against the frame rule in the plan. A codec puts that rule inside a trait implementation, and the frame cap and the `frame_too_large` error still have to be hand-written on top of it.

### `criterion`

- Version 0.8.2, published 2026-02-04. Licence Apache-2.0 OR MIT. MSRV 1.86.
- Maintenance: moved to the `criterion-rs/criterion.rs` organisation, 56 M recent downloads, current.
- Transitive additions: roughly fifteen first-level crates not in the lock, including `anes`, `cast`, `ciborium`, `criterion-plot`, `itertools`, `num-traits`, `oorandom`, `page_size`, `tinytemplate` and `alloca`, before the optional `plotters` and `rayon`.
- Verdict: no. The Phase 1a spike hand-rolls its timing with `std::time::Instant` and dumps JSON, which is what produced the four decision artifacts under `docs/baselines/decisions/`. The plan wants a committed measurement, not a harness, and criterion's statistics would not have changed any of the four verdicts (the closest call, the read-pool curve, is a factor of 2.6 between sizes 1 and 2).

### `nix`

- Version 0.31.3, published 2026-05-11. Licence MIT. MSRV 1.69.
- Maintenance: `nix-rust/nix`, 171 M recent downloads, current.
- Transitive additions: none new (`bitflags`, `cfg-if`, `libc` are in the lock), but only `nix-0.29.0` is in the local cache, so 0.31.3 would download.
- Platform support: both targets, and it is the more ergonomic wrapper of the two.
- Verdict: no. Everything the daemon needs from it is `flock`, `fchmod` and `umask` through `libc`, which is already a direct dependency for exactly that reason, plus `peer_cred` from `tokio`. Adopting `nix` would give the codebase two ways to make the same syscall.

### `jsonrpsee`

- Version 0.26.0, published 2025-08-11, MSRV 1.85, edition 2024. Licence MIT. Note the release order on crates.io: `0.24.11` was published later (2026-05-27) as a backport, so the 0.26 line is a year old without being abandoned.
- Maintenance: `paritytech/jsonrpsee`, 3.8 M recent downloads, an order of magnitude below everything else here.
- Transitive additions: the facade pulls `jsonrpsee-core`, `-types`, `-server`, `-client-transport`, `-http-client`, `-ws-client`, `-proc-macros` plus `tracing`, and under those the HTTP and WebSocket stacks (`hyper`, `http`, `soketto`, `tower`).
- Verdict: no. The plan fixes the transport as hand-written JSON-RPC 2.0 NDJSON over a Unix socket, and this crate is built around HTTP and WebSocket transports. It would own the framing rule, the frame cap and the error mapping that the plan wants owned locally and pinned by fixtures. `interprocess` and `tarpc` are declined for the same reason and are not investigated further.

### `assert_cmd` and `predicates`

- `assert_cmd` 2.2.2, published 2026-05-11, MIT OR Apache-2.0, MSRV 1.85. `predicates` 3.1.4, published 2026-02-11, same licence pair.
- Maintenance: `assert-rs`, current, 16 M and 39 M recent downloads.
- Transitive additions: `bstr`, `predicates`, `predicates-core`, `predicates-tree`, `wait-timeout`; `anstyle` is already in the lock through clap.
- Verdict: no. All eleven existing integration test files drive the binary through `CARGO_BIN_EXE_mp` and `std::process::Command`, and the daemon tests need process control (spawn, signal, wait on a socket) rather than assertion sugar. Adding it would leave two idioms in `tests/`.

## Net ask

1. `thiserror` promoted to a direct dependency of `crates/mp-protocol` when Phase 2 creates it, download-free, train to confirm (this file recommends 2, the plan wrote 1.0.69).
2. Nothing else for phases 0 through 3b.
3. `schemars` stays out and reopens at Phase 7, its first consumer.
4. `notify` stays out; Phase 3b is written against the existing poll.
5. The Phase 2 workspace conversion adds two path members and changes `Cargo.lock` without downloading anything.
