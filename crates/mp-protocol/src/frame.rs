//! Newline framing: one JSON message per line, UTF-8, `\n` terminated.
//!
//! The encoder never emits a raw newline inside a frame, because `serde_json`
//! escapes newlines in string values, which is what makes line framing safe for
//! message bodies and subjects.
//!
//! The decoder is incremental and byte-limited. The cap is inclusive of the
//! terminator and is enforced on the buffer rather than on a completed line, so
//! a client that streams past the limit without ever sending `\n` is cut off
//! instead of being buffered.

use serde::Serialize;
use serde_json::Value;

/// A framing failure. Every variant closes the offending connection and nothing
/// else; [`FrameError::TooLarge`] additionally maps onto the
/// [`crate::ErrorCode::FrameTooLarge`] wire error with the same `{limit, seen}`
/// pair.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// The buffered bytes breached the cap the decoder was built with.
    #[error("frame of {seen} bytes exceeds the {limit} byte limit")]
    TooLarge {
        /// The cap in bytes, terminator included.
        limit: usize,
        /// The byte count that breached it.
        seen: usize,
    },
    /// The frame was not valid UTF-8.
    #[error("frame is not valid UTF-8")]
    InvalidUtf8,
    /// The frame was valid UTF-8 but not valid JSON; carries the parser's
    /// message for the daemon log.
    #[error("frame is not valid JSON: {0}")]
    InvalidJson(String),
}

/// Serialise a message and append the frame terminator.
///
/// The byte cap is a decoder concern, so this does not enforce one: the daemon
/// checks a response against its own limit before writing it.
pub fn encode(msg: &impl Serialize) -> Result<Vec<u8>, FrameError> {
    let mut bytes =
        serde_json::to_vec(msg).map_err(|error| FrameError::InvalidJson(error.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// An incremental newline-framed JSON decoder for one connection.
///
/// Feed it whatever a read returned and it hands back the complete messages,
/// in arrival order, keeping any trailing partial frame for the next call.
#[derive(Debug)]
pub struct Decoder {
    buffer: Vec<u8>,
    /// How much of `buffer` has already been searched for a terminator, so a
    /// long frame arriving in many chunks is scanned once rather than once per
    /// chunk.
    scanned: usize,
    max_bytes: usize,
}

impl Decoder {
    /// Build a decoder capped at `max_bytes` per frame, terminator included.
    ///
    /// The cap is a constructor argument rather than a constant because the
    /// request and response directions are capped separately.
    pub fn new(max_bytes: usize) -> Self {
        Decoder {
            buffer: Vec::new(),
            scanned: 0,
            max_bytes,
        }
    }

    /// The cap this decoder enforces.
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Bytes held for the frame currently being assembled.
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// Append `bytes` and return every frame that completed.
    ///
    /// An error is terminal for the connection, so the buffer is dropped rather
    /// than left holding the remains of a frame nobody will read.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, FrameError> {
        self.buffer.extend_from_slice(bytes);
        match self.decode_buffered() {
            Ok(values) => Ok(values),
            Err(error) => {
                self.buffer.clear();
                self.scanned = 0;
                Err(error)
            }
        }
    }

    fn decode_buffered(&mut self) -> Result<Vec<Value>, FrameError> {
        let mut values = Vec::new();
        while let Some(offset) = self.buffer[self.scanned..].iter().position(|b| *b == b'\n') {
            let end = self.scanned + offset;
            let frame_len = end + 1;
            if frame_len > self.max_bytes {
                return Err(FrameError::TooLarge {
                    limit: self.max_bytes,
                    seen: frame_len,
                });
            }
            let line = std::str::from_utf8(&self.buffer[..end]).map_err(|_| {
                // Report the whole frame, not the first bad byte: the daemon
                // logs one line per rejected frame.
                FrameError::InvalidUtf8
            })?;
            let value: Value = serde_json::from_str(line)
                .map_err(|error| FrameError::InvalidJson(error.to_string()))?;
            values.push(value);
            self.buffer.drain(..frame_len);
            self.scanned = 0;
        }
        self.scanned = self.buffer.len();
        if self.buffer.len() > self.max_bytes {
            return Err(FrameError::TooLarge {
                limit: self.max_bytes,
                seen: self.buffer.len(),
            });
        }
        Ok(values)
    }
}

#[cfg(test)]
mod tests {
    use super::{Decoder, FrameError};
    use serde_json::json;

    #[test]
    fn a_blank_line_is_invalid_json_not_an_empty_frame() {
        let mut decoder = Decoder::new(64);
        assert!(matches!(
            decoder.push(b"\n"),
            Err(FrameError::InvalidJson(_))
        ));
    }

    #[test]
    fn a_carriage_return_before_the_terminator_is_tolerated_by_serde() {
        // A client on a CRLF transport still parses, because `serde_json`
        // treats the trailing `\r` as whitespace.
        let mut decoder = Decoder::new(64);
        assert_eq!(
            decoder.push(b"{\"a\":1}\r\n").unwrap(),
            vec![json!({"a": 1})]
        );
    }

    #[test]
    fn the_buffer_is_dropped_after_an_error() {
        let mut decoder = Decoder::new(64);
        assert!(decoder.push(b"nope\n").is_err());
        assert_eq!(decoder.buffered(), 0);
        assert_eq!(decoder.push(b"{\"a\":1}\n").unwrap(), vec![json!({"a": 1})]);
    }

    #[test]
    fn a_long_frame_arriving_byte_by_byte_is_assembled_once() {
        let frame = b"{\"method\":\"daemon.ping\"}\n";
        let mut decoder = Decoder::new(crate::MAX_REQUEST_BYTES);
        for (index, byte) in frame.iter().enumerate() {
            let values = decoder.push(&[*byte]).unwrap();
            if index + 1 == frame.len() {
                assert_eq!(values.len(), 1);
            } else {
                assert!(values.is_empty());
            }
        }
        assert_eq!(decoder.buffered(), 0);
        assert_eq!(decoder.max_bytes(), 1 << 20);
    }
}
