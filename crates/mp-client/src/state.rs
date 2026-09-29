//! The client's watermark over one daemon instance's revision stream.
//!
//! A client bootstraps, gets a snapshot and the revision it was captured at,
//! and then applies events. [`StateTracker`] answers the only question that
//! decides what to do with each one: apply it, ignore it, or stop and bootstrap
//! again.
//!
//! The rules, in the order they are checked (`docs/daemon-protocol.md`,
//! "Event semantics"):
//!
//! - **A revision from another instance is meaningless**, whatever its number,
//!   because revisions are only comparable within the daemon process that
//!   issued them. That is checked first and is sticky: only a re-bootstrap
//!   against the new instance clears it.
//! - **A poisoned stream applies nothing.** `state.resync_required`
//!   ([`StateTracker::invalidate`]) poisons it, and only
//!   [`StateTracker::rebootstrap`] clears it: a client that kept applying past
//!   it would build a state nothing on the daemon's side corresponds to.
//! - **A revision at or below the watermark is a duplicate** and is dropped
//!   silently. The daemon queues events from the moment a connection registers,
//!   which is before it captures the snapshot, so an event for a change the
//!   snapshot already carries is normal rather than a fault.
//! - **Any revision above the watermark is applied**, however far above.
//!   The daemon's counter is dense, but what one connection receives is not:
//!   coalescing merges two queued events into one carrying the newer revision,
//!   and the older number never travels. A jump is therefore no evidence of a
//!   loss, and the daemon says so itself when something was lost, with a
//!   `state.resync_required` rather than a hole a client has to infer. An
//!   earlier tracker treated any jump above `watermark + 1` as a gap and
//!   re-bootstrapped for nothing on every coalesce.

/// What a client does with one observed event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observe {
    /// In order: apply it, the watermark has moved with it.
    Apply,
    /// At or below the watermark: the snapshot already carries it.
    Duplicate,
    /// The stream is poisoned (a `state.resync_required` was seen and no
    /// bootstrap has followed yet); bootstrap again.
    Gap,
    /// It came from a daemon this client never bootstrapped against.
    InstanceChanged,
}

/// One client's view of one daemon instance's revision stream.
#[derive(Clone, Debug)]
pub struct StateTracker {
    revision: u64,
    instance_id: String,
    /// A `state.resync_required`: nothing is applied until a fresh bootstrap.
    poisoned: bool,
    /// An event from another instance was seen; sticky, and reported ahead of
    /// any revision arithmetic.
    instance_changed: bool,
}

impl StateTracker {
    /// A tracker watermarked at the revision a `state.bootstrap` reported, for
    /// the instance that answered it.
    pub fn new(bootstrap_revision: u64, instance_id: impl Into<String>) -> Self {
        StateTracker {
            revision: bootstrap_revision,
            instance_id: instance_id.into(),
            poisoned: false,
            instance_changed: false,
        }
    }

    /// Classify one event, advancing the watermark when it is in order.
    pub fn observe(&mut self, revision: u64, instance_id: &str) -> Observe {
        if self.instance_changed || instance_id != self.instance_id {
            self.instance_changed = true;
            return Observe::InstanceChanged;
        }
        if self.poisoned {
            return Observe::Gap;
        }
        if revision <= self.revision {
            return Observe::Duplicate;
        }
        self.revision = revision;
        Observe::Apply
    }

    /// The highest revision applied so far.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The instance this tracker bootstrapped against, which it keeps until it
    /// re-bootstraps.
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Whether an event from another instance has been seen since the last
    /// bootstrap, which is sticky until [`StateTracker::rebootstrap`].
    pub fn instance_changed(&self) -> bool {
        self.instance_changed
    }

    /// Whether the only way forward is a fresh `state.bootstrap`.
    pub fn needs_bootstrap(&self) -> bool {
        self.poisoned || self.instance_changed
    }

    /// Discard the tracked state, as a `state.resync_required` notification
    /// asks: nothing is applied until [`StateTracker::rebootstrap`].
    pub fn invalidate(&mut self) {
        self.poisoned = true;
    }

    /// Resume from a fresh bootstrap, clearing a poison and an instance change
    /// alike.
    pub fn rebootstrap(&mut self, revision: u64, instance_id: impl Into<String>) {
        self.revision = revision;
        self.instance_id = instance_id.into();
        self.poisoned = false;
        self.instance_changed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stream in its normal state: in order, then a redundant event the
    /// snapshot already carried, then in order again.
    #[test]
    fn the_watermark_advances_only_on_an_in_order_event() {
        let mut tracker = StateTracker::new(10, "one");
        assert_eq!(tracker.observe(11, "one"), Observe::Apply);
        assert_eq!(tracker.observe(11, "one"), Observe::Duplicate);
        assert_eq!(tracker.revision(), 11);
        assert_eq!(tracker.observe(12, "one"), Observe::Apply);
        assert!(!tracker.needs_bootstrap());
    }

    /// A coalesced stream skips revisions (two queued events merged into one
    /// carrying the newer number), and the protocol permits it: the jump is
    /// applied, not reported as a gap, and the watermark follows it.
    #[test]
    fn a_jump_over_coalesced_revisions_is_applied_not_a_gap() {
        let mut tracker = StateTracker::new(10, "one");
        assert_eq!(tracker.observe(11, "one"), Observe::Apply);
        assert_eq!(tracker.observe(14, "one"), Observe::Apply);
        assert_eq!(tracker.revision(), 14);
        assert!(!tracker.needs_bootstrap());
        assert_eq!(tracker.observe(13, "one"), Observe::Duplicate);
        assert_eq!(tracker.observe(100, "one"), Observe::Apply);
    }

    /// The only gap is the one the daemon announces: after
    /// `state.resync_required` nothing applies, in order or not, until a
    /// fresh bootstrap.
    #[test]
    fn only_a_resync_poisons_the_stream() {
        let mut tracker = StateTracker::new(10, "one");
        tracker.invalidate();
        assert!(tracker.needs_bootstrap());
        assert_eq!(tracker.observe(11, "one"), Observe::Gap);
        assert_eq!(tracker.revision(), 10);
        tracker.rebootstrap(20, "one");
        assert_eq!(tracker.observe(20, "one"), Observe::Duplicate);
        assert_eq!(tracker.observe(22, "one"), Observe::Apply);
    }

    /// An instance change beats the arithmetic, even for a revision that would
    /// otherwise be exactly in order.
    #[test]
    fn an_instance_change_is_checked_before_the_revision() {
        let mut tracker = StateTracker::new(10, "one");
        assert_eq!(tracker.observe(11, "two"), Observe::InstanceChanged);
        assert!(tracker.instance_changed());
        assert_eq!(tracker.revision(), 10);
        assert_eq!(tracker.instance_id(), "one");
        assert_eq!(tracker.observe(11, "one"), Observe::InstanceChanged);
        tracker.rebootstrap(1, "two");
        assert_eq!(tracker.observe(2, "two"), Observe::Apply);
    }
}
