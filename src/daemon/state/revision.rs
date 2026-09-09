//! The three identifiers the canonical state is addressed by.
//!
//! All three are newtypes rather than bare integers and strings: a revision, a
//! connection id and an instance id are all "a number or a string" on the wire
//! and are never interchangeable in the daemon, and the compiler is the cheapest
//! place to find out that they were swapped.

/// A monotonic state revision within one daemon instance.
///
/// Strictly increasing and dense: every committed change takes the next value,
/// which is what lets a client tell "I missed an event" from "nothing has
/// happened". A fresh [`CanonicalState`](super::CanonicalState) starts at
/// `Revision(1)`, so [`Revision::ZERO`] is the client's own pre-bootstrap
/// sentinel and never appears on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision(pub u64);

impl Revision {
    /// The client's "nothing yet" value, which no bootstrap and no event
    /// reports.
    pub const ZERO: Revision = Revision(0);

    /// The number this revision travels as.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// The identity of one daemon process.
///
/// A client that sees an unfamiliar instance has been reconnected to a new
/// daemon and must bootstrap again, whatever the revision says: revisions are
/// only comparable within the instance that issued them.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct InstanceId(pub String);

impl InstanceId {
    /// Build one from anything string-like.
    pub fn new(id: impl Into<String>) -> Self {
        InstanceId(id.into())
    }

    /// The identifier as it travels.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for InstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One connection within this daemon process, and therefore one subscriber of
/// the event fan-out.
///
/// The same number the server hands the
/// [`ClientCtx`](crate::daemon::dispatch::ClientCtx), so a method can address
/// its own caller's queue without reaching back into the transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnectionId(pub u64);

#[cfg(test)]
mod tests {
    use super::*;

    /// The sentinel is below every revision a bootstrap can report, which is
    /// the whole reason a fresh state starts at 1.
    #[test]
    fn zero_is_below_every_committed_revision() {
        assert_eq!(Revision::ZERO.get(), 0);
        assert!(Revision::ZERO < Revision(1));
        assert!(Revision(1) < Revision(2));
    }
}
