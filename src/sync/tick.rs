//! The sequencing of one sync tick: drain, sync, drain (#0114).
//!
//! The mutation queue and the outbox used to be drained only at the head of a
//! tick, so anything the TUI queued while a tick was in flight waited for the
//! next one. A quick tick is short; a full one has been observed at 160 s, and
//! for that whole window a message sent or a mail archived in the TUI stayed
//! local, which is the "changes made in the TUI do not show up in other
//! clients" symptom.
//!
//! The trap the naive fix falls into is the `?` on the sync itself: a tail
//! drain written after it does not run on a failing tick, and the failing ticks
//! are exactly the long ones. So the body's result is carried here rather than
//! propagated, the tail runs either way, and the caller decides what to do with
//! the result afterwards.

use std::future::Future;

/// Run one sync tick with a drain at each end.
///
/// `head` and `body` and `tail` run in that order. The tail runs whether the
/// body succeeded or failed, and the body's result is handed back untouched so
/// the caller can still `?` on it. The two drains each return the status suffix
/// they want appended to the tick's message; a drain that did nothing returns
/// an empty string, so a quiet tail leaves the tick's status text unchanged.
pub async fn run_tick_with_drains<T, Head, HeadFut, Body, BodyFut, Tail, TailFut>(
    head: Head,
    body: Body,
    tail: Tail,
) -> (String, anyhow::Result<T>)
where
    Head: FnOnce() -> HeadFut,
    HeadFut: Future<Output = String>,
    Body: FnOnce() -> BodyFut,
    BodyFut: Future<Output = anyhow::Result<T>>,
    Tail: FnOnce() -> TailFut,
    TailFut: Future<Output = String>,
{
    let head_suffix = head().await;
    let result = body().await;
    let tail_suffix = tail().await;
    (format!("{head_suffix}{tail_suffix}"), result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Record the call order without a runtime: `#[tokio::test]` is
    /// single-threaded, so a `RefCell` is enough and keeps the test readable.
    async fn drain(
        log: &RefCell<Vec<&'static str>>,
        label: &'static str,
        suffix: &'static str,
    ) -> String {
        log.borrow_mut().push(label);
        suffix.to_string()
    }

    #[tokio::test]
    async fn the_tail_drain_runs_after_a_body_that_succeeded() {
        let log = RefCell::new(Vec::new());

        let (suffix, result) = run_tick_with_drains(
            || drain(&log, "head", "; head"),
            || async {
                log.borrow_mut().push("body");
                Ok(7u32)
            },
            || drain(&log, "tail", "; tail"),
        )
        .await;

        assert_eq!(log.into_inner(), vec!["head", "body", "tail"]);
        assert_eq!(suffix, "; head; tail");
        assert_eq!(result.unwrap(), 7);
    }

    #[tokio::test]
    async fn the_tail_drain_runs_after_a_body_that_failed_and_the_error_still_propagates() {
        let log = RefCell::new(Vec::new());

        let (suffix, result) = run_tick_with_drains(
            || drain(&log, "head", "; head"),
            || async {
                log.borrow_mut().push("body");
                Err::<u32, _>(anyhow::anyhow!("login refused"))
            },
            || drain(&log, "tail", "; tail"),
        )
        .await;

        assert_eq!(log.into_inner(), vec!["head", "body", "tail"]);
        assert_eq!(suffix, "; head; tail");
        assert_eq!(format!("{:#}", result.unwrap_err()), "login refused");
    }

    #[tokio::test]
    async fn a_quiet_tail_adds_nothing_to_the_status_text() {
        let log = RefCell::new(Vec::new());

        let (suffix, result) = run_tick_with_drains(
            || drain(&log, "head", ""),
            || async { Ok(()) },
            || drain(&log, "tail", ""),
        )
        .await;

        assert_eq!(suffix, "");
        assert!(result.is_ok());
        assert_eq!(log.into_inner(), vec!["head", "tail"]);
    }
}
