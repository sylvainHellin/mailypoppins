//! Every clipboard write the TUI makes (`y` on a message, `y` on a server
//! search hit, `c` on a contact) goes through [`copy_text`] (PERSO-113).
//!
//! The system clipboard (`arboard`) is tried first. On a headless host, the
//! home server reached over ssh, there is no X11 or Wayland display for it to
//! talk to and it fails with "Failed to access clipboard". The fallback is an
//! OSC 52 escape sequence written to the terminal: the terminal at the far end
//! of the ssh connection (Ghostty, WezTerm, kitty, iTerm2, tmux with
//! `set-clipboard on`) puts the text on the clipboard of the machine the user
//! is sitting at. Nothing answers an OSC 52 write, so the TUI cannot know
//! whether it landed; the status line therefore always shows the text itself
//! when the system clipboard failed, so it can still be selected by hand.

use std::io::{self, Write};

use anyhow::{Context, Result};
use base64::Engine as _;
use ratatui::{backend::CrosstermBackend, Terminal};

use super::app::StatusLevel;

/// Where a copy went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CopyOutcome {
    /// The system clipboard took the text.
    System,
    /// The system clipboard refused; the text was sent to the terminal as an
    /// OSC 52 sequence, which the terminal may or may not honour.
    Osc52 { clipboard_error: String },
    /// The system clipboard refused and the terminal write failed too.
    Failed {
        clipboard_error: String,
        osc52_error: String,
    },
}

impl CopyOutcome {
    /// The status line for a copy of `text`. Every outcome but [`Self::System`]
    /// carries `text`, so the user can still read and select it.
    pub(crate) fn status(&self, text: &str) -> (String, StatusLevel) {
        match self {
            Self::System => (format!("Copied {text}"), StatusLevel::Info),
            Self::Osc52 { .. } => (
                format!("Clipboard unavailable, sent via OSC 52: {text}"),
                StatusLevel::Warning,
            ),
            Self::Failed { osc52_error, .. } => (
                format!("Copy failed ({osc52_error}): {text}"),
                StatusLevel::Error,
            ),
        }
    }
}

/// The OSC 52 "set clipboard" sequence for `text`:
/// `ESC ] 52 ; c ; <base64 of the UTF-8 bytes> BEL`.
///
/// BEL rather than `ESC \` as the terminator, because it is the form every
/// terminal that implements OSC 52 accepts (tmux included).
pub(crate) fn osc52_sequence(text: &str) -> Vec<u8> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let mut seq = Vec::with_capacity(encoded.len() + 8);
    seq.extend_from_slice(b"\x1b]52;c;");
    seq.extend_from_slice(encoded.as_bytes());
    seq.push(0x07);
    seq
}

/// The decision, with both sinks injected so it can be tested without a
/// clipboard or a terminal: the system clipboard first, OSC 52 only when it
/// failed.
fn copy_with_fallback(
    text: &str,
    system: impl FnOnce(&str) -> Result<()>,
    terminal: impl FnOnce(&[u8]) -> io::Result<()>,
) -> CopyOutcome {
    let clipboard_error = match system(text) {
        Ok(()) => return CopyOutcome::System,
        Err(e) => format!("{e:#}"),
    };
    log::info!("system clipboard unavailable ({clipboard_error}), falling back to OSC 52");
    match terminal(&osc52_sequence(text)) {
        Ok(()) => CopyOutcome::Osc52 { clipboard_error },
        Err(e) => {
            let osc52_error = e.to_string();
            log::warn!("copy failed: clipboard: {clipboard_error}; OSC 52: {osc52_error}");
            CopyOutcome::Failed {
                clipboard_error,
                osc52_error,
            }
        }
    }
}

fn copy_to_system_clipboard(text: &str) -> Result<()> {
    let mut clipboard = arboard::Clipboard::new().context("Failed to access clipboard")?;
    clipboard
        .set_text(text)
        .context("Failed to copy to clipboard")?;
    Ok(())
}

/// Copy `text` to the system clipboard, or, when there is none, to the
/// terminal's clipboard over OSC 52.
///
/// The sequence goes through the backend ratatui draws with, so it is
/// serialised with the frame rather than racing it on a second handle; an OSC
/// sequence moves no cursor and paints no cell, so the frame is untouched.
pub(crate) fn copy_text(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    text: &str,
) -> CopyOutcome {
    copy_with_fallback(text, copy_to_system_clipboard, |seq| {
        let out = terminal.backend_mut();
        out.write_all(seq)?;
        out.flush()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn osc52_sequence_encodes_a_known_input() {
        assert_eq!(osc52_sequence("hello"), b"\x1b]52;c;aGVsbG8=\x07".to_vec());
    }

    #[test]
    fn osc52_sequence_encodes_utf8_bytes_and_pads() {
        // "/tmp/ä.md" is 10 bytes (ä is two), so the base64 ends in "==".
        assert_eq!(
            osc52_sequence("/tmp/ä.md"),
            b"\x1b]52;c;L3RtcC/DpC5tZA==\x07".to_vec()
        );
        assert_eq!(osc52_sequence(""), b"\x1b]52;c;\x07".to_vec());
    }

    #[test]
    fn a_working_clipboard_sends_no_escape_sequence() {
        let written = RefCell::new(None::<Vec<u8>>);
        let outcome = copy_with_fallback(
            "mp://work/inbox/1",
            |_| Ok(()),
            |seq| {
                *written.borrow_mut() = Some(seq.to_vec());
                Ok(())
            },
        );
        assert_eq!(outcome, CopyOutcome::System);
        assert!(written.borrow().is_none(), "OSC 52 must not be sent");
        let (line, level) = outcome.status("mp://work/inbox/1");
        assert_eq!(line, "Copied mp://work/inbox/1");
        assert!(matches!(level, StatusLevel::Info));
    }

    #[test]
    fn a_missing_clipboard_falls_back_to_osc52_and_shows_the_text() {
        let written = RefCell::new(None::<Vec<u8>>);
        let outcome = copy_with_fallback(
            "/var/tmp/hit.md",
            |_| anyhow::bail!("Failed to access clipboard"),
            |seq| {
                *written.borrow_mut() = Some(seq.to_vec());
                Ok(())
            },
        );
        assert_eq!(
            outcome,
            CopyOutcome::Osc52 {
                clipboard_error: "Failed to access clipboard".to_string()
            }
        );
        assert_eq!(
            written.borrow().as_deref(),
            Some(osc52_sequence("/var/tmp/hit.md").as_slice())
        );
        let (line, level) = outcome.status("/var/tmp/hit.md");
        assert_eq!(
            line,
            "Clipboard unavailable, sent via OSC 52: /var/tmp/hit.md"
        );
        assert!(matches!(level, StatusLevel::Warning));
    }

    #[test]
    fn a_failed_terminal_write_is_an_error_that_still_shows_the_text() {
        let outcome = copy_with_fallback(
            "a@b.c",
            |_| anyhow::bail!("Failed to access clipboard"),
            |_| Err(io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe")),
        );
        assert_eq!(
            outcome,
            CopyOutcome::Failed {
                clipboard_error: "Failed to access clipboard".to_string(),
                osc52_error: "broken pipe".to_string(),
            }
        );
        let (line, level) = outcome.status("a@b.c");
        assert_eq!(line, "Copy failed (broken pipe): a@b.c");
        assert!(matches!(level, StatusLevel::Error));
    }
}
