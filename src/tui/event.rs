use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyEventKind};

use super::app::Message;

const TICK_RATE: Duration = Duration::from_millis(250);

/// Wait up to the tick rate for one terminal event and convert it to an app
/// message.
///
/// `Ok(None)` means either that nothing arrived within the tick or that what
/// arrived is an event we do not model (a mouse move, a focus change, a
/// paste); the caller treats both as "idle tick".
pub fn poll_event() -> Result<Option<Message>> {
    next_event(TICK_RATE)
}

/// Take one *already queued* terminal event, without waiting (#0108).
///
/// This is the drain step of the coalescing loop in `run_loop`: a held `j`
/// delivers repeats faster than the loop can paint, so the pending backlog is
/// applied to the model first and painted once.
///
/// `Ok(None)` ends the drain. It means the queue is empty, or that the event
/// at its head is one we do not model, in which case the next blocking
/// `poll_event` picks up wherever this left off. Order is preserved: events
/// come off the single crossterm queue in arrival order, so a `Resize`
/// interleaved with keys stays where it was and a `pending_prefix` leader key
/// still sees its own follower next.
pub fn poll_pending_event() -> Result<Option<Message>> {
    next_event(Duration::ZERO)
}

fn next_event(timeout: Duration) -> Result<Option<Message>> {
    if event::poll(timeout)? {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                return Ok(Some(Message::Key(key)));
            }
            Event::Resize(w, h) => {
                return Ok(Some(Message::Resize(w, h)));
            }
            _ => {}
        }
    }
    Ok(None)
}
