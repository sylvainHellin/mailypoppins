mod batch;
/// The login-refusing IMAP server the daemon suites use too (PERSO-296).
#[cfg(test)]
#[path = "../../tests/support/fake_imap.rs"]
pub(crate) mod fake_imap;
mod fetch;
pub mod login_gate;
mod ops;
pub mod pool;
pub mod search;
mod sent;
mod store_sync;
mod watch;

pub use batch::add_flag_in_mailboxes;
pub use fetch::{
    fetch_new_raw_on_session, fetch_raw_by_message_id, fetch_uids_on_session,
    search_stubs_on_session, vanished_uids,
};
pub use login_gate::{login_rejected, LoginRejected};
pub use ops::{
    add_flag_on_server, delete_email_on_server, mark_read_on_server, mark_unread_on_server,
    move_email_on_server, remove_flag_on_server,
};
pub use pool::{checkout, PooledSession, ServerCaps};
pub use search::{
    bracketed_message_id, normalize_message_id, retain_exact_message_id, FetchCriteria,
};
pub use sent::ImapSentMailbox;
pub(crate) use store_sync::list_mailboxes_on;
pub use store_sync::{
    list_mailboxes_detailed, status_counts, sync_mailboxes, ImapBackend, ServerMailbox,
};
// The sync types moved to `crate::sync` with the engine (#0059); re-exported
// here so the call sites that name them through this module keep compiling.
pub use crate::sync::{FetchedRaw, FreshObservation, MailboxState, SyncResult, SyncTarget};
pub use watch::{watch_mailbox, watch_mailbox_since};

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

// ---------------------------------------------------------------------------
// Session opening
// ---------------------------------------------------------------------------

/// Log an IMAP client in with the account's password.
///
/// A `NO` the server meant about the password is a [`LoginRejected`]
/// ([`login_gate::is_rejection`]); every other failure, a `BAD`, a lost
/// connection, a temporary `NO`, stays a plain error that the pool retries.
async fn authenticate_client(
    client: async_imap::Client<ImapStream>,
    imap_config: &ImapConfig,
) -> anyhow::Result<ImapSession> {
    client
        .login(&imap_config.username, &imap_config.password)
        .await
        .map_err(|(e, _client)| login_error(e))
}

/// The error a failed LOGIN surfaces as.
fn login_error(error: async_imap::error::Error) -> anyhow::Error {
    match &error {
        async_imap::error::Error::No(answer) if login_gate::is_rejection(answer) => {
            LoginRejected::new(error.to_string()).into()
        }
        _ => anyhow!("IMAP login failed: {error}"),
    }
}

/// How long the TCP connect of a new IMAP session may take before it is given
/// up on.
///
/// Nothing else here is bounded, and nothing else needs to be: a server that
/// answered the SYN and then went quiet fails its read, while a black-holed
/// host (a dropped route, a firewall that swallows the SYN) leaves the connect
/// itself hanging for the OS default, over two minutes on Linux. Every caller
/// of a session sits on some queue that has to wait for it: a sync thread, the
/// IDLE watcher, or the post-send flag write, which runs after a message has
/// already gone out (#0076).
///
/// Generous on purpose: this is a black-hole guard, not a latency budget, and
/// a slow link on a good day must not lose its connection to it.
const CONNECT_TIMEOUT_SECS: u64 = 30;

/// Open an authenticated session, unless the server refused this login with
/// this password within the hold ([`login_gate`], PERSO-296).
///
/// Every login the daemon makes goes through here: the pool, the IDLE
/// watcher, the Sent mailbox and the one-off sessions of the message methods.
/// A refusal on the list fails at once with the same [`LoginRejected`],
/// without dialling; a refusal the server sends is recorded there, and a
/// login that succeeds clears whatever the list held about its server and
/// user.
pub async fn open_imap_session(imap_config: &ImapConfig) -> anyhow::Result<ImapSession> {
    if let Some(refused) = login_gate::refused(imap_config) {
        return Err(refused.into());
    }
    match dial_imap_session(imap_config).await {
        Ok(session) => {
            login_gate::accepted(imap_config);
            Ok(session)
        }
        Err(e) => match login_rejected(&e) {
            Some(rejected) => Err(login_gate::record(imap_config, rejected).into()),
            None => Err(e),
        },
    }
}

/// Connect, upgrade to TLS and log in, without consulting the list of refused
/// logins and without adding to it: what `config.probe` takes, since a probe
/// is the user asking the server, and what [`open_imap_session`] wraps.
pub(crate) async fn dial_imap_session(imap_config: &ImapConfig) -> anyhow::Result<ImapSession> {
    use crate::timing::TimingSpan;
    use futures::io::{AsyncReadExt, AsyncWriteExt};

    let addr = format!("{}:{}", imap_config.host, imap_config.port);
    let mut span = TimingSpan::with_context("open_imap_session", addr.clone());

    if imap_config.accept_invalid_certs {
        crate::config::ensure_invalid_certs_allowed(&imap_config.host)?;
    }

    let tls = async_native_tls::TlsConnector::new()
        .danger_accept_invalid_certs(imap_config.accept_invalid_certs);

    let mut tcp_stream = async_std::future::timeout(
        std::time::Duration::from_secs(CONNECT_TIMEOUT_SECS),
        async_std::net::TcpStream::connect(&addr),
    )
    .await
    .map_err(|_| {
        anyhow!(
            "Failed to connect to IMAP server: {} did not answer within {}s",
            addr,
            CONNECT_TIMEOUT_SECS
        )
    })?
    .map_err(|e| anyhow!("Failed to connect to IMAP server: {}", e))?;
    span.mark("tcp_connect");

    if imap_config.port == 993 {
        let tls_stream = tls
            .connect(&imap_config.host, tcp_stream)
            .await
            .map_err(|e| anyhow!("TLS handshake failed: {}", e))?;
        span.mark("tls_handshake");

        let client = async_imap::Client::new(ImapStream::passthrough(tls_stream));
        let session = authenticate_client(client, imap_config).await?;
        span.mark("login");
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
        span.mark("greeting");
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
        span.mark("starttls_negotiate");
        info!("IMAP STARTTLS: upgrading to TLS");

        let tls_stream = tls
            .connect(&imap_config.host, tcp_stream)
            .await
            .map_err(|e| anyhow!("STARTTLS TLS handshake failed: {}", e))?;
        span.mark("tls_handshake");

        let client = async_imap::Client::new(ImapStream::with_greeting(tls_stream));
        let session = authenticate_client(client, imap_config).await?;
        span.mark("login");
        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_imap::error::Error;

    /// Only a `NO` about the password is a [`LoginRejected`]; a temporary
    /// `NO`, a `BAD` and a lost connection stay the plain errors the pool
    /// retries, with the text they always had.
    #[test]
    fn only_a_no_about_the_password_is_a_rejected_login() {
        let refused = login_error(Error::No(
            "code: None, info: Some(\"[AUTHENTICATIONFAILED] LOGIN failed.\")".into(),
        ));
        assert!(login_rejected(&refused).is_some());
        assert_eq!(
            refused.to_string(),
            "IMAP login failed: no response: code: None, \
             info: Some(\"[AUTHENTICATIONFAILED] LOGIN failed.\")"
        );
        for passing in [
            Error::No("code: None, info: Some(\"[UNAVAILABLE] down\")".into()),
            Error::Bad("code: None, info: Some(\"LOGIN disabled\")".into()),
            Error::ConnectionLost,
        ] {
            let error = login_error(passing);
            assert!(login_rejected(&error).is_none(), "{error}");
            assert!(error.to_string().starts_with("IMAP login failed: "));
        }
    }
}
