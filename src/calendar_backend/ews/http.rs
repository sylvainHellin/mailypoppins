//! The HTTP transport of the EWS backend (PERSO-256): [`EwsHttp`], the
//! [`EwsFetch`] that posts each envelope to the account's endpoint, logged in
//! over NTLM (`.agents/research/2026-11-13-ews-spike.md`, "Endpoint and
//! login", "The three legs").
//!
//! NTLM authenticates a TCP connection, not a request: the type 1 and the
//! type 3 must go on the one connection the server's type 2 came on, and
//! every request after the login rides that connection with no
//! `Authorization` header. A pooled HTTP client (reqwest) picks a connection
//! per request and opens a new one without a word when the old one is gone,
//! so a type 3 could land on a connection that never saw its challenge and
//! come back 401, which would read as a refused password and count against
//! the account's lockout. The transport therefore speaks HTTP/1.1 itself
//! over one connection it holds (TCP, then TLS through `async-native-tls`,
//! the IMAP client's TLS): a small client for one shape of exchange, a POST
//! with a `Content-Length` body and an answer sized by `Content-Length`, by
//! chunks or by the end of the connection. The server refuses HTTP/2 for
//! NTLM; native TLS offers no ALPN, so the connection is HTTP/1.1.
//!
//! The login, on a new connection:
//!
//! 1. The envelope with `Authorization: NTLM <type 1>`. A 401 carrying
//!    `WWW-Authenticate: NTLM <type 2>` goes on; a 401 that offers no NTLM
//!    challenge is a transport failure naming what the server offers, and
//!    any other answer is the call's answer.
//! 2. The envelope again, on the same connection, with
//!    `Authorization: NTLM <type 3>`. A 401 here is the server refusing the
//!    password: [`EwsFetchError::LoginRefused`], held by the transport so no
//!    later call of it asks the server again. A server that closed the
//!    connection after its challenge gets no type 3 anywhere: the call is a
//!    transport failure, and the next pass logs in afresh.
//!
//! A logged-in connection is reused while it was used in the last
//! [`IDLE_REUSE`], well inside IIS's two-minute keep-alive. A 401 on it means
//! the server dropped its login: the call logs in again on a new connection,
//! once, which is safe since a 401 answer did nothing. Any other failure on
//! it is the call's, not retried: the server may have acted on a write
//! whose answer was lost, and the create's transaction property is what
//! settles that.
//!
//! The user name is sent as configured: `DOMAIN\user` names its domain, a
//! bare name (TUM's) goes with an empty one. The target name is
//! `http/<host>`. Each connect, TLS included, has its timeout, and each call
//! its own over every leg.
//!
//! A refused login is recorded in one place, [`hold`], which keeps the
//! refused password from being asked again for an hour by any later
//! transport of the same login, so a wrong password reaches TUM's directory
//! once an hour rather than at every tick. It stands in for the daemon's
//! login gate (`crate::login_gate`), which will take over that record, the
//! health check and `account.credentials` reporting it, and the user's
//! "set the password again" ending it.

use std::collections::HashMap;
use std::sync::{Mutex as StdMutex, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use futures::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use log::{debug, warn};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::fetch::{EwsFetch, EwsFetchError};
use super::ntlm;
use crate::config::EwsLogin;

/// How long a TCP connect and its TLS handshake may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long one call may take over every leg of its exchange: a calendar
/// view of hundreds of items takes seconds.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// How long a logged-in connection may stand idle and still be used.
pub const IDLE_REUSE: Duration = Duration::from_secs(30);

/// The largest answer head and body the transport reads.
const MAX_HEAD: usize = 64 * 1024;
const MAX_BODY: usize = 64 * 1024 * 1024;

/// The workstation name the type 3 announces; the server reads none.
const WORKSTATION: &str = "MAILYPOPPINS";

/// How long a refused password is not asked again.
pub const REFUSAL_HOLD: Duration = Duration::from_secs(60 * 60);

/// The EWS logins the server refused, held across transports: the one place
/// a refusal is recorded and a held one answered, which the login gate will
/// replace (`crate::login_gate`: a row per login identity and refused secret,
/// 2, 10 and 60 minute holds, the user's action ending one).
///
/// A row is the endpoint's authority, the user name ASCII-lowercased and a
/// hash of the password, never the password; it lives in memory, so a daemon
/// restart forgets it. A password the server took drops its row, and another
/// password has none, so storing a new one is asked at once.
pub mod hold {
    use super::*;

    type Key = (String, String, [u8; 32]);

    fn rows() -> std::sync::MutexGuard<'static, HashMap<Key, (Instant, SystemTime)>> {
        static ROWS: OnceLock<StdMutex<HashMap<Key, (Instant, SystemTime)>>> = OnceLock::new();
        ROWS.get_or_init(StdMutex::default)
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn key(authority: &str, user: &str, password: &str) -> Key {
        (
            authority.to_ascii_lowercase(),
            user.to_ascii_lowercase(),
            Sha256::digest(password.as_bytes()).into(),
        )
    }

    /// When the held refusal of `key` ends, while it holds.
    pub(super) fn held(key: &Key) -> Option<SystemTime> {
        let mut rows = rows();
        match rows.get(key) {
            Some((at, wall)) if at.elapsed() < REFUSAL_HOLD => Some(*wall + REFUSAL_HOLD),
            Some(_) => {
                rows.remove(key);
                None
            }
            None => None,
        }
    }

    /// The server refused `key`'s password.
    pub(super) fn refused(key: Key) {
        rows().insert(key, (Instant::now(), SystemTime::now()));
    }

    /// The server took `key`'s password.
    pub(super) fn accepted(key: &Key) {
        rows().remove(key);
    }
}

/// How long a connect and a whole call may take.
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    pub connect: Duration,
    pub request: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            connect: CONNECT_TIMEOUT,
            request: REQUEST_TIMEOUT,
        }
    }
}

/// A byte stream the transport speaks HTTP over.
trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

/// Where the envelopes go.
#[derive(Debug, Clone)]
struct Endpoint {
    tls: bool,
    host: String,
    port: u16,
    /// The path and query of the request line.
    target: String,
    /// The `Host` header: the host, with the port when it is not the
    /// scheme's.
    authority: String,
}

impl Endpoint {
    fn parse(url: &str, allow_plain: bool) -> Result<Self> {
        let url = reqwest::Url::parse(url).with_context(|| format!("{url:?} is not a URL"))?;
        let tls = match url.scheme() {
            "https" => true,
            "http" if allow_plain => false,
            other => bail!("an EWS endpoint is an https URL, not {other}"),
        };
        let host = url
            .host_str()
            .ok_or_else(|| anyhow!("the EWS endpoint names no host"))?
            .to_string();
        let port = url
            .port_or_known_default()
            .ok_or_else(|| anyhow!("the EWS endpoint names no port"))?;
        let mut target = url.path().to_string();
        if let Some(query) = url.query() {
            target.push('?');
            target.push_str(query);
        }
        let authority = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.clone(),
        };
        Ok(Endpoint {
            tls,
            host,
            port,
            target,
            authority,
        })
    }
}

/// `DOMAIN\user` as its user and domain; any other name is a user with an
/// empty domain.
fn split_user(username: &str) -> (&str, &str) {
    match username.split_once('\\') {
        Some((domain, user)) => (user, domain),
        None => (username, ""),
    }
}

/// One connection, with what it read past the last answer.
struct Connection {
    stream: Box<dyn Stream>,
    buffered: Vec<u8>,
    last_used: Instant,
}

/// One answer, read whole.
struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    /// The server ends the connection after this answer.
    close: bool,
}

impl Answer {
    fn headers_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.headers
            .iter()
            .filter(move |(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The NTLM challenge of a `WWW-Authenticate: NTLM <token>`, decoded.
    fn ntlm_challenge(&self) -> Option<Vec<u8>> {
        self.headers_named("WWW-Authenticate")
            .flat_map(|value| value.split(','))
            .find_map(|offer| {
                let (scheme, token) = offer.trim().split_once(' ')?;
                if !scheme.eq_ignore_ascii_case("NTLM") {
                    return None;
                }
                base64::engine::general_purpose::STANDARD
                    .decode(token.trim())
                    .ok()
            })
    }

    /// What the server offers to log in with, for a refusal that names it.
    fn offers(&self) -> String {
        let offers: Vec<&str> = self.headers_named("WWW-Authenticate").collect();
        if offers.is_empty() {
            "no WWW-Authenticate".to_string()
        } else {
            format!("WWW-Authenticate: {}", offers.join(", "))
        }
    }
}

/// What the transport holds between calls.
#[derive(Default)]
struct State {
    /// The logged-in connection.
    connection: Option<Connection>,
    /// The server's refusal of the login, which every later call answers.
    refused: Option<String>,
}

/// The EWS transport of one account: its endpoint, its login and the one
/// connection it holds. It lives for one sync pass or one write, as the
/// backend over it does.
pub struct EwsHttp {
    endpoint: Endpoint,
    user: String,
    domain: String,
    password: String,
    /// The NTLM target name, `http/<host>`.
    spn: String,
    timeouts: Timeouts,
    state: Mutex<State>,
}

impl EwsHttp {
    /// The transport of `login`, logging in with `password`, the secret its
    /// `password_from` names.
    pub fn new(login: &EwsLogin, password: String) -> Result<Self> {
        Self::build(
            &login.endpoint,
            &login.username,
            password,
            Timeouts::default(),
            false,
        )
    }

    /// A transport to a plain-HTTP `url`, for the tests' scripted server.
    #[cfg(test)]
    fn plain_for_test(url: &str, username: &str, password: &str, timeouts: Timeouts) -> Self {
        Self::build(url, username, password.to_string(), timeouts, true).unwrap()
    }

    fn build(
        url: &str,
        username: &str,
        password: String,
        timeouts: Timeouts,
        allow_plain: bool,
    ) -> Result<Self> {
        let endpoint = Endpoint::parse(url, allow_plain)?;
        let (user, domain) = split_user(username.trim());
        Ok(EwsHttp {
            spn: format!("http/{}", endpoint.host.to_ascii_lowercase()),
            endpoint,
            user: user.to_string(),
            domain: domain.to_string(),
            password,
            timeouts,
            state: Mutex::new(State::default()),
        })
    }

    /// Open a connection, TLS for an https endpoint.
    async fn connect(&self) -> Result<Connection> {
        let Endpoint { host, port, .. } = &self.endpoint;
        let seconds = self.timeouts.connect.as_secs_f32();
        let tcp = tokio::time::timeout(
            self.timeouts.connect,
            async_std::net::TcpStream::connect((host.as_str(), *port)),
        )
        .await
        .map_err(|_| anyhow!("{host}:{port} did not answer within {seconds}s"))?
        .with_context(|| format!("cannot connect to {host}:{port}"))?;
        let stream: Box<dyn Stream> = if self.endpoint.tls {
            let tls = tokio::time::timeout(
                self.timeouts.connect,
                async_native_tls::TlsConnector::new().connect(host.as_str(), tcp),
            )
            .await
            .map_err(|_| anyhow!("the TLS handshake with {host} took over {seconds}s"))?
            .with_context(|| format!("the TLS handshake with {host} failed"))?;
            Box::new(tls)
        } else {
            Box::new(tcp)
        };
        Ok(Connection {
            stream,
            buffered: Vec::new(),
            last_used: Instant::now(),
        })
    }

    /// Post `envelope` on `connection`, with the NTLM message `ntlm` as its
    /// `Authorization` when there is one, and read the answer whole.
    async fn round_trip(
        &self,
        connection: &mut Connection,
        envelope: &str,
        ntlm: Option<&[u8]>,
    ) -> Result<Answer> {
        let mut head = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: mailypoppins/{}\r\n\
             Content-Type: text/xml; charset=utf-8\r\nContent-Length: {}\r\n\
             Connection: keep-alive\r\n",
            self.endpoint.target,
            self.endpoint.authority,
            env!("CARGO_PKG_VERSION"),
            envelope.len()
        );
        if let Some(message) = ntlm {
            head.push_str("Authorization: NTLM ");
            head.push_str(&base64::engine::general_purpose::STANDARD.encode(message));
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        let stream = &mut connection.stream;
        stream.write_all(head.as_bytes()).await.map_err(io_error)?;
        stream
            .write_all(envelope.as_bytes())
            .await
            .map_err(io_error)?;
        stream.flush().await.map_err(io_error)?;
        let answer = read_answer(connection).await?;
        connection.last_used = Instant::now();
        Ok(answer)
    }

    /// The call's result from the server's answer: the body of a 200, and of
    /// a 500, which carries a SOAP fault; any other status is named.
    fn result_of(answer: Answer) -> Result<String, EwsFetchError> {
        match answer.status {
            200 | 500 => String::from_utf8(answer.body).map_err(|_| {
                EwsFetchError::Transport(anyhow!(
                    "the Exchange server answered text that is not UTF-8"
                ))
            }),
            status => Err(EwsFetchError::Status {
                status,
                body: String::from_utf8_lossy(&answer.body).into_owned(),
            }),
        }
    }

    /// Log in on a new connection with `envelope` as the request, keeping
    /// the connection on success.
    async fn log_in(&self, state: &mut State, envelope: &str) -> Result<String, EwsFetchError> {
        let transport = EwsFetchError::Transport;
        let row = hold::key(&self.endpoint.authority, &self.user, &self.password);
        if let Some(until) = hold::held(&row) {
            let at = chrono::DateTime::<chrono::Local>::from(until).format("%H:%M");
            let why = format!(
                "refused earlier; this password is asked again at {at}, or at once after a new \
                 one is stored"
            );
            state.refused = Some(why.clone());
            return Err(EwsFetchError::LoginRefused(why));
        }
        let mut connection = self.connect().await.map_err(transport)?;
        let negotiate = ntlm::negotiate_message();
        let first = self
            .round_trip(&mut connection, envelope, Some(&negotiate))
            .await
            .map_err(|error| transport(error.context("the NTLM negotiation failed")))?;
        if first.status != 401 {
            if first.status == 200 && !first.close {
                state.connection = Some(connection);
            }
            return Self::result_of(first);
        }
        let Some(challenge) = first.ntlm_challenge() else {
            return Err(transport(anyhow!(
                "the Exchange server offers no NTLM login ({})",
                first.offers()
            )));
        };
        if first.close {
            return Err(transport(anyhow!(
                "the Exchange server closed the connection after its NTLM challenge"
            )));
        }
        let credentials = ntlm::Credentials {
            user: &self.user,
            domain: &self.domain,
            password: &self.password,
            workstation: WORKSTATION,
        };
        let authenticate = ntlm::authenticate_message(
            &negotiate,
            &challenge,
            &credentials,
            &self.spn,
            ntlm::Nonces::random(),
        )
        .map_err(|error| transport(error.context("unreadable NTLM challenge")))?;
        let second = self
            .round_trip(&mut connection, envelope, Some(&authenticate))
            .await
            .map_err(|error| transport(error.context("the NTLM login failed")))?;
        if second.status == 401 {
            let why = format!("HTTP 401 after the NTLM login as {}", self.user);
            warn!(
                "[calendar] the Exchange server at {} refused the login of {}",
                self.endpoint.host, self.user
            );
            state.refused = Some(why.clone());
            hold::refused(row);
            return Err(EwsFetchError::LoginRefused(why));
        }
        hold::accepted(&row);
        if !second.close {
            state.connection = Some(connection);
        }
        Self::result_of(second)
    }

    /// One call: on the logged-in connection while it is fresh, else, or
    /// when that connection lost its login, through a new login.
    async fn exchange(&self, state: &mut State, envelope: &str) -> Result<String, EwsFetchError> {
        if let Some(mut connection) = state.connection.take() {
            if connection.last_used.elapsed() < IDLE_REUSE {
                let answer = self
                    .round_trip(&mut connection, envelope, None)
                    .await
                    .map_err(EwsFetchError::Transport)?;
                if answer.status != 401 {
                    if !answer.close {
                        state.connection = Some(connection);
                    }
                    return Self::result_of(answer);
                }
                debug!(
                    "[calendar] the Exchange server at {} dropped the login of its connection; \
                     logging in again",
                    self.endpoint.host
                );
            }
        }
        self.log_in(state, envelope).await
    }
}

impl EwsFetch for EwsHttp {
    /// Post `envelope` and answer the body.
    ///
    /// A refused login is answered as [`EwsFetchError::LoginRefused`] here
    /// and by every later call of this transport without a request, and by
    /// every later transport of the same login and password within
    /// [`REFUSAL_HOLD`] ([`hold`]).
    async fn post(&self, envelope: &str) -> Result<String, EwsFetchError> {
        let mut state = self.state.lock().await;
        if let Some(why) = &state.refused {
            return Err(EwsFetchError::LoginRefused(why.clone()));
        }
        let seconds = self.timeouts.request.as_secs_f32();
        match tokio::time::timeout(self.timeouts.request, self.exchange(&mut state, envelope)).await
        {
            Ok(result) => result,
            Err(_) => {
                state.connection = None;
                Err(EwsFetchError::Transport(anyhow!(
                    "the Exchange server did not answer within {seconds}s"
                )))
            }
        }
    }
}

/// An I/O failure on a connection, a server that dropped it named so.
fn io_error(error: std::io::Error) -> anyhow::Error {
    use std::io::ErrorKind::{BrokenPipe, ConnectionAborted, ConnectionReset, UnexpectedEof};
    match error.kind() {
        BrokenPipe | ConnectionAborted | ConnectionReset | UnexpectedEof => {
            anyhow!("the Exchange server closed the connection ({error})")
        }
        _ => anyhow::Error::new(error),
    }
}

/// Read more of `connection` into its buffer; `false` at the end of the
/// stream.
async fn fill(connection: &mut Connection) -> Result<bool> {
    let mut chunk = [0u8; 16 * 1024];
    let read = connection.stream.read(&mut chunk).await.map_err(io_error)?;
    connection.buffered.extend_from_slice(&chunk[..read]);
    Ok(read > 0)
}

/// The next `count` bytes of `connection`.
async fn take(connection: &mut Connection, count: usize) -> Result<Vec<u8>> {
    while connection.buffered.len() < count {
        if !fill(connection).await? {
            bail!("the Exchange server closed the connection in the middle of its answer");
        }
    }
    Ok(connection.buffered.drain(..count).collect())
}

/// The next line of `connection`, without its CRLF.
async fn line(connection: &mut Connection) -> Result<String> {
    loop {
        if let Some(at) = connection.buffered.windows(2).position(|w| w == b"\r\n") {
            let line: Vec<u8> = connection.buffered.drain(..at + 2).take(at).collect();
            return String::from_utf8(line).context("an answer line that is not text");
        }
        if connection.buffered.len() > MAX_HEAD {
            bail!("an answer line longer than {MAX_HEAD} bytes");
        }
        if !fill(connection).await? {
            bail!("the Exchange server closed the connection in the middle of its answer");
        }
    }
}

/// Read one answer whole: its status line and headers, then its body by
/// `Content-Length`, by chunks, or to the end of the connection. An interim
/// `1xx` answer is skipped.
async fn read_answer(connection: &mut Connection) -> Result<Answer> {
    loop {
        if connection.buffered.is_empty() && !fill(connection).await? {
            bail!("the Exchange server closed the connection before it answered");
        }
        let status_line = line(connection).await?;
        let mut parts = status_line.splitn(3, ' ');
        let version = parts.next().unwrap_or_default().to_string();
        let status: u16 = parts
            .next()
            .and_then(|code| code.parse().ok())
            .filter(|_| version.starts_with("HTTP/1."))
            .ok_or_else(|| anyhow!("not an HTTP/1.1 answer: {status_line:?}"))?;
        let mut headers = Vec::new();
        let mut head = status_line.len();
        loop {
            let header = line(connection).await?;
            if header.is_empty() {
                break;
            }
            head += header.len();
            if head > MAX_HEAD {
                bail!("an answer head longer than {MAX_HEAD} bytes");
            }
            let (key, value) = header
                .split_once(':')
                .ok_or_else(|| anyhow!("an unreadable answer header {header:?}"))?;
            headers.push((key.trim().to_string(), value.trim().to_string()));
        }
        if (100..200).contains(&status) {
            continue;
        }
        let named = |name: &str| {
            headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.to_ascii_lowercase())
        };
        let mut close = named("Connection").is_some_and(|value| value.contains("close"))
            || (version == "HTTP/1.0"
                && !named("Connection").is_some_and(|value| value.contains("keep-alive")));
        let body = if named("Transfer-Encoding").is_some_and(|value| value.contains("chunked")) {
            read_chunks(connection).await?
        } else if let Some(length) = named("Content-Length") {
            let length: usize = length
                .parse()
                .map_err(|_| anyhow!("an unreadable Content-Length {length:?}"))?;
            if length > MAX_BODY {
                bail!("an answer of {length} bytes, over the {MAX_BODY} the transport reads");
            }
            take(connection, length).await?
        } else if status == 204 || status == 304 {
            Vec::new()
        } else {
            close = true;
            while fill(connection).await? {
                if connection.buffered.len() > MAX_BODY {
                    bail!("an answer over the {MAX_BODY} bytes the transport reads");
                }
            }
            std::mem::take(&mut connection.buffered)
        };
        return Ok(Answer {
            status,
            headers,
            body,
            close,
        });
    }
}

/// A chunked body, its trailers read and dropped.
async fn read_chunks(connection: &mut Connection) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let size_line = line(connection).await?;
        let size = size_line.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size, 16)
            .map_err(|_| anyhow!("an unreadable chunk size {size_line:?}"))?;
        if size == 0 {
            while !line(connection).await?.is_empty() {}
            return Ok(body);
        }
        if body.len() + size > MAX_BODY {
            bail!("an answer over the {MAX_BODY} bytes the transport reads");
        }
        body.extend(take(connection, size).await?);
        if !take(connection, 2).await?.eq(b"\r\n") {
            bail!("a chunk that does not end with CRLF");
        }
    }
}

#[cfg(test)]
mod tests;
