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
//! The wait blocks the calling thread. That is fine inside an async test: the
//! daemon is another process, and nothing in the test needs to run until it
//! answers.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

/// Poll until the daemon on `socket` answers `daemon.status`, or fail the test
/// after `within`, polling every `tick`.
pub fn wait_until_answering(socket: &Path, within: Duration, tick: Duration) {
    let start = Instant::now();
    loop {
        if status_round_trip(socket, start + within) {
            return;
        }
        assert!(
            start.elapsed() < within,
            "the daemon on {} never answered daemon.status within {within:?}",
            socket.display()
        );
        std::thread::sleep(tick);
    }
}

/// Ask the daemon at `socket` for `daemon.status` and answer whether it
/// replied before `deadline`.
///
/// `daemon.status` is lifecycle surface, answered without a handshake. Any
/// failure - nothing listening, a connection closed during startup, a reply
/// that is not this request's result - is `false`, and the caller polls again.
fn status_round_trip(socket: &Path, deadline: Instant) -> bool {
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return false;
    };
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() || stream.set_read_timeout(Some(left)).is_err() {
        return false;
    }
    let request = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"daemon.status\",\"params\":{}}\n";
    if stream.write_all(request).is_err() {
        return false;
    }
    let mut line = String::new();
    if BufReader::new(&stream).read_line(&mut line).is_err() {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(&line)
        .map(|reply| reply["id"] == 1 && reply.get("result").is_some())
        .unwrap_or(false)
}
