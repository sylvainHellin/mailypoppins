//! The lifecycle surface's typed members.
//!
//! `daemon.status` is answered before the handshake and its result is read as
//! JSON by `mp daemon status`; the members a client may branch on are typed
//! here, starting with `connections` (PERSO-221).

use serde::{Deserialize, Serialize};

/// One bootstrapped connection's outbound event queue, as `daemon.status`
/// lists it in `connections`.
///
/// The queue is the one buffer between the daemon's fan-out and that
/// connection's socket: a client that reads keeps it empty, and one that stops
/// reading grows it up to its caps, after which the daemon discards the queued
/// domain events, asks the client to bootstrap again and sets `overflowed`
/// until it does. A connection that never bootstrapped receives no events and
/// is not listed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ConnectionQueue {
    /// The connection's id, the `connection` its `state.bootstrap` answered.
    pub connection: u64,
    /// The kind its `initialize` announced: `cli`, `tui` or `gui`.
    pub client: String,
    /// How many events are queued for it and not yet written.
    pub events: u64,
    /// What those events weigh, in bytes of payload.
    pub bytes: u64,
    /// Whether the queue overflowed and the client has been asked to
    /// bootstrap again: until it does, the daemon drops every domain event
    /// for it.
    pub overflowed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// The committed `daemon.status` fixture's `connections` decode and
    /// re-encode to the same JSON.
    #[test]
    fn the_status_fixtures_connections_round_trip() {
        let raw = include_str!("../fixtures/daemon.status.response.json");
        let response: Value = serde_json::from_str(raw).expect("the fixture is JSON");
        let connections = response["result"]["connections"].clone();
        let typed: Vec<ConnectionQueue> =
            serde_json::from_value(connections.clone()).expect("the connections decode");
        assert_eq!(typed.len(), 2);
        assert!(typed[1].overflowed);
        assert_eq!(serde_json::to_value(&typed).unwrap(), connections);
    }
}
