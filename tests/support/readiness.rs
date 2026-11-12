//! Daemon readiness for a test that spawned `mp daemon run` itself.
//!
//! Self-contained on purpose: [`super::parity`] reaches it as
//! `support::readiness`, and a test binary that does not declare `mod support;`
//! includes this one file with `#[path = "support/readiness.rs"] mod readiness;`
//! instead of carrying its own copy.
//!
//! # Why a connect is not readiness
//!
//! The daemon binds its socket, and the kernel queues connections from that
//! instant, *before* it writes `daemon.pid` and `daemon.json` and before it
//! serves (`src/daemon/lifecycle.rs`, startup steps 7 to 9). A connect-only
//! wait therefore returns inside that window, and a test that then reads a
//! runtime file, or races the daemon's first log lines, flakes whenever the
//! daemon is descheduled there (PERSO-142). The daemon answers only from its
//! serve loop, which starts after the files are written, so a `daemon.status`
//! round trip - what `mp daemon start` itself calls ready - means all of
//! startup is done.
//!
//! # Why an answer is not readiness either
//!
//! The daemon answers before it has opened any account's store (PERSO-109):
//! each runtime start opens its store off the accept loop, and until it has, a
//! store method on that account is refused with `account_not_ready` and
//! `state: "opening"`. A test that calls one right after the daemon answered
//! would race that start, so the wait also waits until `daemon.status` lists
//! no account as `opening`. A daemon that starts no runtimes lists no accounts,
//! and passes at once.
//!
//! The wait blocks the calling thread. That is fine inside an async test: the
//! daemon is another process, and nothing in the test needs to run until it
//! answers.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

/// Poll until the daemon on `socket` answers `daemon.status` with no account
/// still opening its store, or fail the test after `within`, polling every
/// `tick`.
pub fn wait_until_answering(socket: &Path, within: Duration, tick: Duration) {
    let start = Instant::now();
    let mut answered = None;
    loop {
        if let Some(status) = status_round_trip(socket, start + within) {
            if !opening(&status) {
                return;
            }
            answered = Some(status);
        }
        assert!(
            start.elapsed() < within,
            "the daemon on {} never answered daemon.status with every account opened \
             within {within:?}; it last answered {answered:?}",
            socket.display()
        );
        std::thread::sleep(tick);
    }
}

/// Whether a `daemon.status` result lists an account still opening its store.
fn opening(status: &serde_json::Value) -> bool {
    status["accounts"]
        .as_array()
        .is_some_and(|accounts| accounts.iter().any(|a| a["state"] == "opening"))
}

/// Ask the daemon at `socket` for `daemon.status` and answer its result when
/// it replied before `deadline`.
///
/// `daemon.status` is lifecycle surface, answered without a handshake. Any
/// failure - nothing listening, a connection closed during startup, a reply
/// that is not this request's result - is `None`, and the caller polls again.
fn status_round_trip(socket: &Path, deadline: Instant) -> Option<serde_json::Value> {
    let mut stream = UnixStream::connect(socket).ok()?;
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() || stream.set_read_timeout(Some(left)).is_err() {
        return None;
    }
    let request = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"daemon.status\",\"params\":{}}\n";
    stream.write_all(request).ok()?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).ok()?;
    let mut reply = serde_json::from_str::<serde_json::Value>(&line).ok()?;
    if reply["id"] != 1 {
        return None;
    }
    reply.get_mut("result").map(serde_json::Value::take)
}
