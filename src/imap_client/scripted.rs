//! An IMAP server played from a script, for the unit tests of the commands
//! the client sends by hand, since the repository has no in-process server
//! that answers them.
//!
//! Each read that finds the current reply used up hands out the next, and
//! records what the client wrote since the previous one, so a test sees which
//! bytes went out before which answer came back.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

/// What the client wrote before each read, one entry per read.
pub(crate) type Turns = Arc<Mutex<Vec<String>>>;

#[derive(Debug)]
pub(crate) struct ScriptedServer {
    replies: VecDeque<&'static str>,
    reply: Vec<u8>,
    written: Vec<u8>,
    turns: Turns,
}

impl futures::io::AsyncRead for ScriptedServer {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.reply.is_empty() {
            let sent = String::from_utf8_lossy(&std::mem::take(&mut self.written)).into_owned();
            self.turns.lock().unwrap().push(sent);
            let Some(next) = self.replies.pop_front() else {
                return Poll::Ready(Ok(0));
            };
            self.reply = next.as_bytes().to_vec();
        }
        let n = self.reply.len().min(buf.len());
        buf[..n].copy_from_slice(&self.reply[..n]);
        self.reply.drain(..n);
        Poll::Ready(Ok(n))
    }
}

impl futures::io::AsyncWrite for ScriptedServer {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.written.extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// A session logged in to a server that plays `replies`, the first of which
/// answers the `LOGIN`, and the turns it records.
pub(crate) async fn session(
    replies: &[&'static str],
) -> (async_imap::Session<ScriptedServer>, Turns) {
    let turns = Turns::default();
    let server = ScriptedServer {
        replies: replies.iter().copied().collect(),
        reply: Vec::new(),
        written: Vec::new(),
        turns: turns.clone(),
    };
    let session = async_imap::Client::new(server)
        .login("user", "secret")
        .await
        .map_err(|e| e.0)
        .unwrap();
    (session, turns)
}

/// What the client wrote before each read since the `LOGIN`.
pub(crate) fn after_login(turns: &Turns) -> Vec<String> {
    turns.lock().unwrap().split_off(1)
}
