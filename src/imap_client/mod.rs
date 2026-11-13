mod batch;
/// The login-refusing IMAP server the daemon suites use too (PERSO-296).
#[cfg(test)]
#[path = "../../tests/support/fake_imap.rs"]
pub(crate) mod fake_imap;
mod fetch;
mod ops;
pub mod pool;
#[cfg(test)]
mod scripted;
pub mod search;
mod sent;
mod store_sync;
mod uidplus;
mod watch;

pub use batch::add_flag_in_mailboxes;
/// The arrival arithmetic, so the engine's fake backend answers a pass that
/// ended partway the way this transport would (PERSO-300).
#[cfg(test)]
pub(crate) use fetch::{arrival_coverage, ceiling};
pub use fetch::{
    fetch_raw_by_message_id, fetch_uids_on_session, search_stubs_on_session, vanished_uids,
};
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
    folder_identities, list_mailboxes_detailed, status_counts, sync_mailboxes, FolderIdentity,
    ImapBackend, ServerMailbox,
};
pub(crate) use uidplus::append as append_with_uid;
// The sync types moved to `crate::sync` with the engine (#0059); re-exported
// here so the call sites that name them through this module keep compiling.
pub use crate::sync::{FetchedMessage, FreshObservation, MailboxState, SyncResult, SyncTarget};
pub use watch::{watch_mailbox, watch_mailbox_since};

use std::fmt;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context as TaskContext, Poll};

use anyhow::anyhow;
use futures::io::{AsyncRead, AsyncWrite};
use log::info;

use crate::config::ImapConfig;
use crate::login_gate::{self, login_rejected, Login, LoginRejected};
use mp_protocol::account::RejectedLogin;

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
/// ([`is_rejection`]); every other failure, a `BAD`, a lost
/// connection, a `NO` with any other response code, stays a plain error that
/// the pool retries.
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
        async_imap::error::Error::No(answer) if is_rejection(answer) => {
            LoginRejected::new(RejectedLogin::Imap, error.to_string()).into()
        }
        _ => anyhow!("IMAP login failed: {error}"),
    }
}

/// The RFC 5530 response codes that make a `NO` to LOGIN an answer about the
/// credential: the password or the user is wrong, the user may not act as
/// the one named, or the password expired.
const CREDENTIAL_CODES: [&str; 3] = ["AUTHENTICATIONFAILED", "AUTHORIZATIONFAILED", "EXPIRED"];

/// Whether a `NO` to LOGIN, as async-imap renders it, refuses the password.
///
/// async-imap renders the answer as `code: {code:?}, info: {info:?}`. A code
/// imap-proto parses is the `Some(..)` of the first half (`ALERT`, which
/// Gmail sends to a correct password over its connection and bandwidth
/// limits, renders as `code: Some(Alert), info: Some("Too many ...")`); it
/// does not know RFC 5530's, which stay at the start of the text
/// (`code: None, info: Some("[AUTHENTICATIONFAILED] ...")`).
///
/// A `NO` without a code (Exchange's `NO LOGIN failed.`) refuses, and so
/// does one whose code is `AUTHENTICATIONFAILED`, `AUTHORIZATIONFAILED` or
/// `EXPIRED`, ignoring case as IMAP atoms do. Any other code, the temporary
/// ones (`UNAVAILABLE`, `INUSE`, `LIMIT`, `SERVERBUG`), `ALERT`,
/// `CONTACTADMIN` or one nobody registered, says nothing certain about the
/// password: the login is retried as any failed connection is, and nothing
/// is recorded.
fn is_rejection(answer: &str) -> bool {
    let Some(rest) = answer.strip_prefix("code: None, info: ") else {
        // A code imap-proto parsed, or an answer of an unknown shape: no
        // certain answer about the password.
        return false;
    };
    let Some(text) = rest
        .strip_prefix("Some(\"")
        .and_then(|text| text.strip_suffix("\")"))
    else {
        // `info: None`: a bare `NO`.
        return rest == "None";
    };
    let Some(code) = text.strip_prefix('[') else {
        return true;
    };
    let code = code.split([']', ' ']).next().unwrap_or_default();
    CREDENTIAL_CODES
        .iter()
        .any(|credential| code.eq_ignore_ascii_case(credential))
}

/// How long the TCP connect of a new IMAP session may take before it is given
/// up on.
///
/// A black-holed host (a dropped route, a firewall that swallows the SYN)
/// leaves the connect hanging for the OS default, over two minutes on Linux,
/// and a server that answered the SYN and then went quiet leaves every read
/// after it hanging for as long as the connection stays up, which
/// [`login_gate::LOGIN_DEADLINE`] bounds from the connect to the login's
/// answer (PERSO-296). Every caller
/// of a session sits on some queue that has to wait for it: a sync thread, the
/// IDLE watcher, or the post-send flag write, which runs after a message has
/// already gone out (#0076).
///
/// Generous on purpose: this is a black-hole guard, not a latency budget, and
/// a slow link on a good day must not lose its connection to it.
const CONNECT_TIMEOUT_SECS: u64 = 30;

/// Open an authenticated session, unless the server refused this login with
/// this password within the hold ([`crate::login_gate`], PERSO-296).
///
/// Every login the daemon makes goes through here: the pool, the IDLE
/// watcher, the Sent mailbox and the one-off sessions of the message methods.
/// A refusal on the list fails at once with the same [`LoginRejected`],
/// without dialling; a refusal the server sends is recorded there, and a
/// login that succeeds clears whatever the list held about its server and
/// user.
pub async fn open_imap_session(imap_config: &ImapConfig) -> anyhow::Result<ImapSession> {
    open_imap_session_within(imap_config, login_gate::LOGIN_DEADLINE).await
}

/// [`open_imap_session`] with the deadline of its handshake and login handed
/// in, so a test need not wait out [`login_gate::LOGIN_DEADLINE`].
pub(crate) async fn open_imap_session_within(
    imap_config: &ImapConfig,
    deadline: std::time::Duration,
) -> anyhow::Result<ImapSession> {
    let login = Login::Imap(imap_config.login());
    let mut attempt = Attempt {
        dial: login_gate::dial(&login, &imap_config.password)?,
        login_sent: AtomicBool::new(false),
        reported: false,
    };
    match dial_imap_session_within(imap_config, deadline, &attempt.login_sent).await {
        Ok(session) => {
            attempt.accepted();
            Ok(session)
        }
        Err(e) => match login_rejected(&e) {
            Some(rejected) => Err(attempt.refused(rejected).into()),
            None => {
                attempt.unanswered();
                Err(e)
            }
        },
    }
}

/// One gated IMAP dial, reported to the gate whatever ends it (PERSO-307).
///
/// Once the LOGIN may have gone out, every outcome other than the server
/// taking it or refusing the password is no answer
/// ([`login_gate::Dial::unanswered`]): the deadline, a connection closed or
/// reset, a `BAD`, a `NO` about something else, and the task cancelled
/// (the future dropped), since the answer the dial never read may well have
/// been a refusal. That is a step of the hold of a password the server
/// refused before, and nothing for one never refused. Before the LOGIN,
/// nothing reached the server, and the dial is only let go of.
struct Attempt {
    dial: login_gate::Dial,
    /// Set as the LOGIN step starts, before its write: the command may leave
    /// even when the write fails, and a login counted with its bytes unsent
    /// is the safe side of one sent and never counted. Atomic, as the dial's
    /// future is `Send`.
    login_sent: AtomicBool,
    reported: bool,
}

impl Attempt {
    fn accepted(&mut self) {
        self.reported = true;
        self.dial.accepted();
    }

    fn refused(&mut self, rejected: &LoginRejected) -> LoginRejected {
        self.reported = true;
        self.dial.refused(rejected)
    }

    /// No answer about the password: [`login_gate::Dial::unanswered`] once
    /// the LOGIN may have gone out, and nothing before, when the dial is
    /// only dropped.
    fn unanswered(&mut self) {
        self.reported = true;
        if self.login_sent.load(Ordering::SeqCst) {
            self.dial.unanswered();
        }
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        if !self.reported {
            self.unanswered();
        }
    }
}

/// Connect, upgrade to TLS and log in, without consulting the list of refused
/// logins and without adding to it: what `config.probe` takes, since a probe
/// is the user asking the server, and what [`open_imap_session`] wraps.
pub(crate) async fn dial_imap_session(imap_config: &ImapConfig) -> anyhow::Result<ImapSession> {
    dial_imap_session_within(
        imap_config,
        login_gate::LOGIN_DEADLINE,
        &AtomicBool::new(false),
    )
    .await
}

/// [`dial_imap_session`] with the deadline of everything after the TCP
/// connect handed in: the greeting and STARTTLS, the TLS handshake and the
/// login together have `deadline` from the moment the connection is open,
/// and a step still waiting at its end fails with an error naming the server
/// and the step, which is no refusal (PERSO-296). `login_sent` is set as the
/// LOGIN step starts, before the command is written, so [`open_imap_session`]
/// can count a LOGIN that got no answer, by the deadline or otherwise, as a
/// step of a refused password's hold (PERSO-307).
async fn dial_imap_session_within(
    imap_config: &ImapConfig,
    deadline: std::time::Duration,
    login_sent: &AtomicBool,
) -> anyhow::Result<ImapSession> {
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
    let by = Deadline {
        addr: &addr,
        end: std::time::Instant::now() + deadline,
        deadline,
    };

    if imap_config.port == 993 {
        let tls_stream = by
            .step(Step::TlsHandshake, async {
                tls.connect(&imap_config.host, tcp_stream)
                    .await
                    .map_err(|e| anyhow!("TLS handshake failed: {}", e))
            })
            .await?;
        span.mark("tls_handshake");

        let client = async_imap::Client::new(ImapStream::passthrough(tls_stream));
        let session = by
            .login(authenticate_client(client, imap_config), login_sent)
            .await?;
        span.mark("login");
        Ok(session)
    } else {
        info!("IMAP STARTTLS: connecting to {} (plaintext first)", addr);

        let mut buf = [0u8; 4096];
        let n = by
            .step(Step::Greeting, async {
                tcp_stream
                    .read(&mut buf)
                    .await
                    .map_err(|e| anyhow!("Failed to read IMAP greeting: {}", e))
            })
            .await?;
        let greeting = String::from_utf8_lossy(&buf[..n]);
        if !greeting.starts_with("* ") {
            return Err(anyhow!("Unexpected IMAP greeting: {}", greeting.trim()));
        }
        span.mark("greeting");
        info!("IMAP STARTTLS: got greeting, sending STARTTLS");

        let n = by
            .step(Step::StartTls, async {
                tcp_stream
                    .write_all(b"a0 STARTTLS\r\n")
                    .await
                    .map_err(|e| anyhow!("Failed to send STARTTLS: {}", e))?;
                tcp_stream
                    .flush()
                    .await
                    .map_err(|e| anyhow!("Failed to flush STARTTLS: {}", e))?;
                tcp_stream
                    .read(&mut buf)
                    .await
                    .map_err(|e| anyhow!("Failed to read STARTTLS response: {}", e))
            })
            .await?;
        let response = String::from_utf8_lossy(&buf[..n]);
        if !response.contains("OK") {
            return Err(anyhow!("STARTTLS rejected by server: {}", response.trim()));
        }
        span.mark("starttls_negotiate");
        info!("IMAP STARTTLS: upgrading to TLS");

        let tls_stream = by
            .step(Step::TlsHandshake, async {
                tls.connect(&imap_config.host, tcp_stream)
                    .await
                    .map_err(|e| anyhow!("STARTTLS TLS handshake failed: {}", e))
            })
            .await?;
        span.mark("tls_handshake");

        let client = async_imap::Client::new(ImapStream::with_greeting(tls_stream));
        let session = by
            .login(authenticate_client(client, imap_config), login_sent)
            .await?;
        span.mark("login");
        Ok(session)
    }
}

/// The deadline of a dial's steps after its TCP connect.
struct Deadline<'a> {
    /// The server, `host:port`.
    addr: &'a str,
    /// When the steps must be done.
    end: std::time::Instant,
    /// How long they had, for the error.
    deadline: std::time::Duration,
}

impl Deadline<'_> {
    /// Run `step`, failing it once the deadline has passed.
    async fn step<T>(
        &self,
        step: Step,
        run: impl std::future::Future<Output = anyhow::Result<T>>,
    ) -> anyhow::Result<T> {
        let left = self
            .end
            .saturating_duration_since(std::time::Instant::now());
        async_std::future::timeout(left, run)
            .await
            .map_err(|_| StepTimedOut {
                addr: self.addr.to_string(),
                step,
                deadline: self.deadline,
            })?
    }

    /// Run the LOGIN step `run`, setting `login_sent` as it starts, before
    /// `run` writes the command.
    async fn login<T>(
        &self,
        run: impl std::future::Future<Output = anyhow::Result<T>>,
        login_sent: &AtomicBool,
    ) -> anyhow::Result<T> {
        self.step(Step::Login, async {
            login_sent.store(true, Ordering::SeqCst);
            run.await
        })
        .await
    }
}

/// The steps of a dial after its TCP connect, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// The plaintext greeting before STARTTLS.
    Greeting,
    /// The STARTTLS command and its answer.
    StartTls,
    /// The TLS handshake.
    TlsHandshake,
    /// The LOGIN command and its answer: from here on the password may have
    /// reached the server.
    Login,
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Step::Greeting => "greeting",
            Step::StartTls => "STARTTLS",
            Step::TlsHandshake => "TLS handshake",
            Step::Login => "login",
        })
    }
}

/// A dial step the server did not answer by the deadline.
#[derive(Debug)]
struct StepTimedOut {
    /// The server, `host:port`.
    addr: String,
    step: Step,
    deadline: std::time::Duration,
}

impl fmt::Display for StepTimedOut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "IMAP server {} did not answer the {} within {:?} of the connection",
            self.addr, self.step, self.deadline
        )
    }
}

impl std::error::Error for StepTimedOut {}

#[cfg(test)]
mod tests {
    use super::*;
    use async_imap::error::Error;

    /// The answers async-imap renders a tagged `NO` to LOGIN as: the code is
    /// `None` because imap-proto does not parse RFC 5530's, and they sit at
    /// the start of the text.
    fn no(info: &str) -> String {
        format!("code: None, info: Some({info:?})")
    }

    /// `NO [ALERT] ...`: imap-proto parses `ALERT`, so it is the code and the
    /// text is what follows it.
    fn alert(info: &str) -> String {
        format!("code: Some(Alert), info: Some({info:?})")
    }

    /// A `NO` without a response code, Exchange's `NO LOGIN failed.`, and the
    /// three RFC 5530 codes about the credential refuse the password,
    /// whatever their case.
    #[test]
    fn a_no_without_a_code_or_with_a_credential_code_refuses_the_password() {
        for refusal in [
            no("LOGIN failed."),
            "code: None, info: None".to_string(),
            no("[AUTHENTICATIONFAILED] Invalid credentials (Failure)"),
            no("[AUTHORIZATIONFAILED] No such authorization-ID"),
            no("[EXPIRED] That password isn't valid any more"),
            no("[authenticationFailed] LOGIN failed."),
            no("[EXPIRED]"),
        ] {
            assert!(is_rejection(&refusal), "{refusal}");
        }
    }

    /// Any other response code, one imap-proto parses (`ALERT`, which Gmail
    /// sends to a correct password over its connection and bandwidth limits)
    /// or one it leaves at the start of the text (RFC 5530's temporary codes,
    /// `CONTACTADMIN`, a code nobody registered), is no answer about the
    /// password, so it is not recorded.
    #[test]
    fn a_no_with_any_other_code_does_not_refuse_the_password() {
        for passing in [
            alert("Too many simultaneous connections. (Failure)"),
            alert("Account exceeded command or bandwidth limits. (Failure)"),
            no("[UNAVAILABLE] User's backend down for maintenance"),
            no("[INUSE] Mailbox in use"),
            no("[LIMIT] Too many connections"),
            no("[SERVERBUG] Internal error"),
            no("[CONTACTADMIN] Ask your administrator"),
            no("[unavailable] try again later"),
            no("[X-SOMETHING-NEW] LOGIN failed."),
            no("[AUTHENTICATIONFAILED-ISH] LOGIN failed."),
        ] {
            assert!(!is_rejection(&passing), "{passing}");
        }
    }

    /// Only a `NO` about the password is a [`LoginRejected`]; a `NO` with
    /// another code, a `BAD` and a lost connection stay the plain errors the
    /// pool retries, with the text they always had.
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
            Error::No(
                "code: Some(Alert), info: Some(\"Too many simultaneous connections.\")".into(),
            ),
            Error::Bad("code: None, info: Some(\"LOGIN disabled\")".into()),
            Error::ConnectionLost,
        ] {
            let error = login_error(passing);
            assert!(login_rejected(&error).is_none(), "{error}");
            assert!(error.to_string().starts_with("IMAP login failed: "));
        }
    }
}
