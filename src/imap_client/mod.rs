mod batch;
mod fetch;
mod ops;
pub mod search;
mod sync;
mod watch;

pub use batch::{batch_archive_emails_locally, batch_delete_emails_locally};
pub use fetch::{
    fetch_emails, fetch_emails_on_session, fetch_new_emails_on_session,
    fetch_server_message_ids, fetch_server_message_ids_on_session,
};
pub use ops::{
    append_to_sent_folder, archive_email_locally, archive_email_on_server,
    delete_email_locally, delete_email_on_server,
    mark_read_on_server, mark_unread_on_server, update_read_status_locally,
    get_message_id_from_file,
};
pub use search::{parse_search_query, FetchCriteria};
pub use sync::{list_mailboxes, sync_mailboxes, FreshObservation, SyncResult, SyncTarget};
pub use watch::watch_mailbox;

use std::fmt;
use std::io;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};

use anyhow::anyhow;
use futures::io::{AsyncRead, AsyncWrite};
use log::info;

use crate::config::ImapConfig;

pub type ImapSession = async_imap::Session<ImapStream>;

// ---------------------------------------------------------------------------
// Stream wrapper for STARTTLS support
// ---------------------------------------------------------------------------

/// A wrapper around a TLS stream that optionally injects a fake IMAP greeting.
pub struct ImapStream {
    prefix: Vec<u8>,
    prefix_pos: usize,
    inner: async_native_tls::TlsStream<async_std::net::TcpStream>,
}

impl ImapStream {
    fn passthrough(inner: async_native_tls::TlsStream<async_std::net::TcpStream>) -> Self {
        Self {
            prefix: Vec::new(),
            prefix_pos: 0,
            inner,
        }
    }

    fn with_greeting(inner: async_native_tls::TlsStream<async_std::net::TcpStream>) -> Self {
        Self {
            prefix: b"* OK STARTTLS ready\r\n".to_vec(),
            prefix_pos: 0,
            inner,
        }
    }
}

impl AsyncRead for ImapStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        if self.prefix_pos < self.prefix.len() {
            let remaining = &self.prefix[self.prefix_pos..];
            let n = remaining.len().min(buf.len());
            buf[..n].copy_from_slice(&remaining[..n]);
            self.prefix_pos += n;
            return Poll::Ready(Ok(n));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for ImapStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_close(cx)
    }
}

impl Unpin for ImapStream {}

impl fmt::Debug for ImapStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImapStream")
            .field("prefix_remaining", &(self.prefix.len() - self.prefix_pos))
            .finish()
    }
}

pub async fn open_imap_session(imap_config: &ImapConfig) -> anyhow::Result<ImapSession> {
    use futures::io::{AsyncReadExt, AsyncWriteExt};

    let tls = async_native_tls::TlsConnector::new()
        .danger_accept_invalid_certs(imap_config.accept_invalid_certs);
    let addr = format!("{}:{}", imap_config.host, imap_config.port);

    let mut tcp_stream = async_std::net::TcpStream::connect(&addr)
        .await
        .map_err(|e| anyhow!("Failed to connect to IMAP server: {}", e))?;

    if imap_config.port == 993 {
        let tls_stream = tls
            .connect(&imap_config.host, tcp_stream)
            .await
            .map_err(|e| anyhow!("TLS handshake failed: {}", e))?;

        let client = async_imap::Client::new(ImapStream::passthrough(tls_stream));

        let session = client
            .login(&imap_config.username, &imap_config.password)
            .await
            .map_err(|e| anyhow!("IMAP login failed: {}", e.0))?;

        Ok(session)
    } else {
        info!("IMAP STARTTLS: connecting to {} (plaintext first)", addr);

        let mut buf = [0u8; 4096];
        let n = tcp_stream
            .read(&mut buf)
            .await
            .map_err(|e| anyhow!("Failed to read IMAP greeting: {}", e))?;
        let greeting = String::from_utf8_lossy(&buf[..n]);
        if !greeting.starts_with("* ") {
            return Err(anyhow!("Unexpected IMAP greeting: {}", greeting.trim()));
        }
        info!("IMAP STARTTLS: got greeting, sending STARTTLS");

        tcp_stream
            .write_all(b"a0 STARTTLS\r\n")
            .await
            .map_err(|e| anyhow!("Failed to send STARTTLS: {}", e))?;
        tcp_stream
            .flush()
            .await
            .map_err(|e| anyhow!("Failed to flush STARTTLS: {}", e))?;

        let n = tcp_stream
            .read(&mut buf)
            .await
            .map_err(|e| anyhow!("Failed to read STARTTLS response: {}", e))?;
        let response = String::from_utf8_lossy(&buf[..n]);
        if !response.contains("OK") {
            return Err(anyhow!("STARTTLS rejected by server: {}", response.trim()));
        }
        info!("IMAP STARTTLS: upgrading to TLS");

        let tls_stream = tls
            .connect(&imap_config.host, tcp_stream)
            .await
            .map_err(|e| anyhow!("STARTTLS TLS handshake failed: {}", e))?;

        let client = async_imap::Client::new(ImapStream::with_greeting(tls_stream));

        let session = client
            .login(&imap_config.username, &imap_config.password)
            .await
            .map_err(|e| anyhow!("IMAP login failed: {}", e.0))?;

        Ok(session)
    }
}
