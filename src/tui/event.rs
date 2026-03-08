use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyEventKind};

use super::app::Message;

const TICK_RATE: Duration = Duration::from_millis(250);

/// Poll for terminal events and convert them to app messages.
pub fn poll_event() -> Result<Option<Message>> {
    if event::poll(TICK_RATE)? {
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
