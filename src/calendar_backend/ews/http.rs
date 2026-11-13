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
//! Every login goes through the daemon's login gate ([`crate::login_gate`],
//! PERSO-296) as the EWS login of the account, which shares its row with the
//! IMAP login of the same user and password. The gate is asked before the
//! connect: a password it holds is [`EwsFetchError::LoginRefused`] without a
//! connection, saying where the password was refused and when it is asked
//! again. A 401 to the type 3 is reported refused, an answer past it
//! accepted, and a login that has no answer by [`Timeouts::login`] (the
//! gate's [`LOGIN_DEADLINE`], from the end of the TCP connect) is reported
//! unanswered once its type 3 was written, which counts as a step of the
//! hold of a password refused before; before the type 3 it counts nothing,
//! as does any other failure.

use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use futures::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use log::{debug, warn};
use mp_protocol::account::RejectedLogin;
use tokio::sync::Mutex;

use super::fetch::{EwsFetch, EwsFetchError};
use super::ntlm;
use crate::config::EwsLogin;
use crate::login_gate::{self, Login, LoginRejected, LOGIN_DEADLINE};

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

/// How long a connect and a whole call may take.
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    pub connect: Duration,
    pub request: Duration,
    /// How long a login may go unanswered from the end of its TCP connect,
    /// over the TLS handshake and both NTLM legs.
    pub login: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            connect: CONNECT_TIMEOUT,
            request: REQUEST_TIMEOUT,
            login: LOGIN_DEADLINE,
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

/// How a handshake ended short of a refusal: the server answered the type 1
/// without asking for a login, or answered the type 3.
enum Handshake {
    NotAsked(Answer, Connection),
    Answered(Answer, Connection),
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
    /// The login the gate keeps refusals of.
    login: Login,
    user: String,
    domain: String,
    password: String,
    /// The NTLM target name, `http/<host>`.
    spn: String,
    timeouts: Timeouts,
    state: Mutex<State>,
}

impl EwsHttp {
    /// The transport of `account`'s `login`, logging in with `password`, the
    /// secret its `password_from` names.
    pub fn new(account: &str, login: &EwsLogin, password: String) -> Result<Self> {
        Self::build(
            &login.endpoint,
            &login.username,
            password,
            Timeouts::default(),
            false,
            Login::ews(account, login),
        )
    }

    /// A transport to a plain-HTTP `url`, for the tests' scripted server,
    /// whose gate login is the EWS login of an account of its own logging
    /// in as `username`.
    #[cfg(test)]
    fn plain_for_test(url: &str, username: &str, password: &str, timeouts: Timeouts) -> Self {
        let endpoint = Endpoint::parse(url, true).unwrap();
        let login = Login::Ews(
            crate::config::ServerLogin {
                account: format!("ews-test-{username}"),
                host: endpoint.host.clone(),
                port: endpoint.port,
                username: split_user(username).0.to_string(),
            },
            crate::config::EwsPasswordSource::Imap,
        );
        Self::build(url, username, password.to_string(), timeouts, true, login).unwrap()
    }

    fn build(
        url: &str,
        username: &str,
        password: String,
        timeouts: Timeouts,
        allow_plain: bool,
        login: Login,
    ) -> Result<Self> {
        let endpoint = Endpoint::parse(url, allow_plain)?;
        let (user, domain) = split_user(username.trim());
        Ok(EwsHttp {
            login,
            spn: format!("http/{}", endpoint.host.to_ascii_lowercase()),
            endpoint,
            user: user.to_string(),
            domain: domain.to_string(),
            password,
            timeouts,
            state: Mutex::new(State::default()),
        })
    }

    /// Open a TCP connection to the endpoint.
    async fn connect_tcp(&self) -> Result<async_std::net::TcpStream> {
        let Endpoint { host, port, .. } = &self.endpoint;
        let seconds = self.timeouts.connect.as_secs_f32();
        tokio::time::timeout(
            self.timeouts.connect,
            async_std::net::TcpStream::connect((host.as_str(), *port)),
        )
        .await
        .map_err(|_| anyhow!("{host}:{port} did not answer within {seconds}s"))?
        .with_context(|| format!("cannot connect to {host}:{port}"))
    }

    /// `tcp` as a connection, TLS for an https endpoint.
    async fn secure(&self, tcp: async_std::net::TcpStream) -> Result<Connection> {
        let host = &self.endpoint.host;
        let stream: Box<dyn Stream> = if self.endpoint.tls {
            let tls = async_native_tls::TlsConnector::new()
                .connect(host.as_str(), tcp)
                .await
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
    /// `Authorization` when there is one.
    async fn write_request(
        &self,
        connection: &mut Connection,
        envelope: &str,
        ntlm: Option<&[u8]>,
    ) -> Result<()> {
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
        stream.flush().await.map_err(io_error)
    }

    /// Post `envelope` on `connection` ([`EwsHttp::write_request`]) and read
    /// the answer whole.
    async fn round_trip(
        &self,
        connection: &mut Connection,
        envelope: &str,
        ntlm: Option<&[u8]>,
    ) -> Result<Answer> {
        self.write_request(connection, envelope, ntlm).await?;
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

    /// The NTLM handshake on `tcp` with `envelope` as the request: the TLS
    /// handshake, the type 1 and, when the server challenges, the type 3.
    /// `type3_written` is set once the type 3 went out whole, which is when
    /// the password may have reached the server.
    async fn handshake(
        &self,
        tcp: async_std::net::TcpStream,
        envelope: &str,
        type3_written: &mut bool,
    ) -> Result<Handshake, EwsFetchError> {
        let transport = EwsFetchError::Transport;
        let mut connection = self.secure(tcp).await.map_err(transport)?;
        let negotiate = ntlm::negotiate_message();
        let first = self
            .round_trip(&mut connection, envelope, Some(&negotiate))
            .await
            .map_err(|error| transport(error.context("the NTLM negotiation failed")))?;
        if first.status != 401 {
            return Ok(Handshake::NotAsked(first, connection));
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
        self.write_request(&mut connection, envelope, Some(&authenticate))
            .await
            .map_err(|error| transport(error.context("the NTLM login failed")))?;
        *type3_written = true;
        let second = read_answer(&mut connection)
            .await
            .map_err(|error| transport(error.context("the NTLM login failed")))?;
        connection.last_used = Instant::now();
        Ok(Handshake::Answered(second, connection))
    }

    /// Log in on a new connection with `envelope` as the request, through
    /// the login gate, keeping the connection on success.
    async fn log_in(&self, state: &mut State, envelope: &str) -> Result<String, EwsFetchError> {
        let mut dial = login_gate::dial(&self.login, &self.password).map_err(|held| {
            let why = held.to_string();
            state.refused = Some(why.clone());
            EwsFetchError::LoginRefused(why)
        })?;
        // Any failure from here on that is not one of the three reported
        // below drops the dial, which counts nothing.
        let tcp = self.connect_tcp().await.map_err(EwsFetchError::Transport)?;
        let mut type3_written = false;
        let handshake = tokio::time::timeout(
            self.timeouts.login,
            self.handshake(tcp, envelope, &mut type3_written),
        )
        .await;
        let (answer, connection, logged_in) = match handshake {
            Err(_) => {
                if type3_written {
                    dial.unanswered();
                }
                return Err(EwsFetchError::Transport(anyhow!(
                    "the Exchange server did not answer the NTLM login within {}s",
                    self.timeouts.login.as_secs_f32()
                )));
            }
            Ok(Err(error)) => return Err(error),
            Ok(Ok(Handshake::NotAsked(answer, connection))) => (answer, connection, false),
            Ok(Ok(Handshake::Answered(answer, _))) if answer.status == 401 => {
                warn!(
                    "[calendar] the Exchange server at {} refused the login of {}",
                    self.endpoint.host, self.user
                );
                let why = format!("HTTP 401 after the NTLM login as {}", self.user);
                let rejected = dial.refused(&LoginRejected::new(RejectedLogin::Ews, why));
                let why = rejected.to_string();
                state.refused = Some(why.clone());
                return Err(EwsFetchError::LoginRefused(why));
            }
            Ok(Ok(Handshake::Answered(answer, connection))) => {
                // An answer past the login (a SOAP answer or fault) says the
                // server took it; any other status says nothing about it.
                if matches!(answer.status, 200 | 500) {
                    dial.accepted();
                }
                (answer, connection, true)
            }
        };
        // A connection the server logged in stays, whatever it answered; one
        // that answered without a login only when the answer was a success.
        if !answer.close && (logged_in || answer.status == 200) {
            state.connection = Some(connection);
        }
        Self::result_of(answer)
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
    /// every later login of the same user and password while the gate holds
    /// it.
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
        // `body.len() + size` would overflow on a hostile size; `body` never
        // holds more than the cap, so the difference cannot.
        if size > MAX_BODY - body.len() {
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
