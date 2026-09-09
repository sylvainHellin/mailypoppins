//! The client's watermark over one daemon instance's revision stream.
//!
//! A client bootstraps, gets a snapshot and the revision it was captured at,
//! and then applies events. [`StateTracker`] answers the only question that
//! decides what to do with each one: apply it, ignore it, or stop and bootstrap
//! again.
//!
//! The three rules, in the order they are checked:
//!
//! - **A revision from another instance is meaningless**, whatever its number,
//!   because revisions are only comparable within the daemon process that
//!   issued them. That is checked first and is sticky: only a re-bootstrap
//!   against the new instance clears it.
//! - **A revision at or below the watermark is a duplicate** and is dropped
//!   silently. The daemon queues events from the moment a connection registers,
//!   which is before it captures the snapshot, so an event for a change the
//!   snapshot already carries is normal rather than a fault.
//! - **A revision more than one above the watermark is a gap**, which poisons
//!   the stream: nothing is applied, the watermark does not move, and every
//!   later event is refused until [`StateTracker::rebootstrap`]. A client that
//!   kept applying past a gap would build a state nothing on the daemon's side
//!   corresponds to.

/// What a client does with one observed event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observe {
    /// In order: apply it, the watermark has moved with it.
    Apply,
    /// At or below the watermark: the snapshot already carries it.
    Duplicate,
    /// A revision was missed; bootstrap again.
    Gap,
    /// It came from a daemon this client never bootstrapped against.
    InstanceChanged,
}

/// One client's view of one daemon instance's revision stream.
#[derive(Clone, Debug)]
pub struct StateTracker {
    revision: u64,
    instance_id: String,
    /// A gap or a `state.resync_required`: nothing is applied until a fresh
    /// bootstrap.
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
        if revision > self.revision + 1 {
            self.poisoned = true;
            return Observe::Gap;
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

    /// Whether the only way forward is a fresh `state.bootstrap`.
    pub fn needs_bootstrap(&self) -> bool {
        self.poisoned || self.instance_changed
    }

    /// Discard the tracked state, as a `state.resync_required` notification
    /// asks: the same poisoning a gap causes, applied on the daemon's word.
    pub fn invalidate(&mut self) {
        self.poisoned = true;
    }

    /// Resume from a fresh bootstrap, clearing a gap and an instance change
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

    /// An instance change beats the arithmetic, even for a revision that would
    /// otherwise be exactly in order.
    #[test]
    fn an_instance_change_is_checked_before_the_revision() {
        let mut tracker = StateTracker::new(10, "one");
        assert_eq!(tracker.observe(11, "two"), Observe::InstanceChanged);
        assert_eq!(tracker.revision(), 10);
        assert_eq!(tracker.instance_id(), "one");
        assert_eq!(tracker.observe(11, "one"), Observe::InstanceChanged);
        tracker.rebootstrap(1, "two");
        assert_eq!(tracker.observe(2, "two"), Observe::Apply);
    }
}
