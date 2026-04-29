//! Structured timing instrumentation for performance analysis.
//!
//! `TimingSpan` records a labelled span with optional intermediate marks.
//! All output is emitted via `log::info!` with a `[TIMING]` prefix so that
//! grepping the log file gives a clean per-phase breakdown.
//!
//! Time values are reported as wall-clock milliseconds. Marks are cumulative
//! since the span started, plus the delta from the previous mark. On drop,
//! the total elapsed is logged.
//!
//! Typical use:
//!
//! ```ignore
//! use crate::timing::TimingSpan;
//!
//! let mut span = TimingSpan::new("sync_mailboxes");
//! // ... do work ...
//! span.mark("select");
//! // ... more work ...
//! span.mark("search");
//! // span dropped here -> logs total
//! ```
//!
//! With context:
//!
//! ```ignore
//! let mut span = TimingSpan::with_context("fetch_new_emails", "INBOX (proton)");
//! span.mark("uid_search");
//! ```

use std::time::Instant;

use log::info;

/// A timing span that logs elapsed milliseconds for a labelled operation.
///
/// Construct with [`TimingSpan::new`] or [`TimingSpan::with_context`].
/// Call [`mark`](Self::mark) at phase boundaries. The total elapsed is
/// logged automatically when the span is dropped.
pub struct TimingSpan {
    name: String,
    context: Option<String>,
    start: Instant,
    last_mark: Instant,
}

impl TimingSpan {
    /// Start a new timing span with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        let now = Instant::now();
        let name = name.into();
        info!("[TIMING] {} start", name);
        Self {
            name,
            context: None,
            start: now,
            last_mark: now,
        }
    }

    /// Start a new timing span with a name and a context tag (e.g. mailbox name,
    /// account name). The context is included in every log line for this span.
    pub fn with_context(name: impl Into<String>, context: impl Into<String>) -> Self {
        let now = Instant::now();
        let name = name.into();
        let context = context.into();
        info!("[TIMING] {} [{}] start", name, context);
        Self {
            name,
            context: Some(context),
            start: now,
            last_mark: now,
        }
    }

    /// Record an intermediate phase boundary. Logs the elapsed time since the
    /// previous mark (or span start) and the cumulative elapsed since start.
    pub fn mark(&mut self, label: &str) {
        let now = Instant::now();
        let delta_ms = now.duration_since(self.last_mark).as_millis();
        let total_ms = now.duration_since(self.start).as_millis();
        match &self.context {
            Some(ctx) => info!(
                "[TIMING] {} [{}] {}: +{} ms (total {} ms)",
                self.name, ctx, label, delta_ms, total_ms
            ),
            None => info!(
                "[TIMING] {} {}: +{} ms (total {} ms)",
                self.name, label, delta_ms, total_ms
            ),
        }
        self.last_mark = now;
    }
}

impl Drop for TimingSpan {
    fn drop(&mut self) {
        let total_ms = self.start.elapsed().as_millis();
        match &self.context {
            Some(ctx) => info!(
                "[TIMING] {} [{}] done: {} ms",
                self.name, ctx, total_ms
            ),
            None => info!("[TIMING] {} done: {} ms", self.name, total_ms),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn span_records_total_on_drop() {
        // Just exercise the code path; we cannot easily assert on log output
        // without pulling in a log capture crate. The Drop impl must not panic.
        let span = TimingSpan::new("test_span");
        sleep(Duration::from_millis(1));
        drop(span);
    }

    #[test]
    fn span_with_context_does_not_panic() {
        let mut span = TimingSpan::with_context("test", "ctx");
        span.mark("phase1");
        sleep(Duration::from_millis(1));
        span.mark("phase2");
    }
}
