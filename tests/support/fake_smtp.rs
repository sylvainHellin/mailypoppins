//! An SMTP server that takes a whole conversation over STARTTLS and records
//! what it was given (PERSO-199, PERSO-210).
//!
//! `send::build_smtp_transport` is TLS-only: port 465 is implicit TLS and every
//! other port is STARTTLS. This server speaks STARTTLS (RFC 3207) on a free
//! localhost port with the throwaway identity [`super::fake_imap`] presents
//! (`fake_imap.p12`, CN `localhost`, SAN `127.0.0.1`), which an account reaches
//! with `accept_invalid_certs = true`, allowed for a loopback host only. The
//! implicit-TLS branch is chosen by port 465 alone, which a test cannot bind,
//! so it is out of reach.
//!
//! It answers as Postfix does by default: no AUTH and no mail before
//! STARTTLS, no mail before AUTH, MAIL before RCPT before DATA (RFC 5321
//! 4.1.4), each reply with its enhanced status code (RFC 3463). It records
//! every AUTH PLAIN and AUTH LOGIN with the username and password it carried
//! ([`RecordingSmtpServer::logins`]), and every message a DATA brought in,
//! its envelope and its bytes with the dot-stuffing undone (RFC 5321 4.5.2,
//! [`RecordingSmtpServer::deliveries`]), and every `RCPT TO` with the refusal
//! it got ([`RecordingSmtpServer::rcpts`]).
//!
//! A test changes the answer to any step of the conversation
//! ([`RecordingSmtpServer::answer`]), or to a step of the transaction of one
//! recipient ([`RecordingSmtpServer::answer_for`]): a reply of its own (a
//! `535 5.7.8` to AUTH, a `550 5.1.1` to RCPT), no answer at all while the
//! connection stays open ([`SmtpAnswer::Stall`]), or the connection closed
//! ([`SmtpAnswer::HangUp`]), for good or for the next few transactions
//! ([`RecordingSmtpServer::answer_for_next`]). [`RecordingSmtpServer::reached`] counts the
//! times the server came to each step, so a test waits on a stall without
//! sleeping.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_std::net::{TcpListener, TcpStream};
use base64::Engine;
use futures::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// The identity the server presents, and its export password: the fake IMAP
/// server's, so the repository carries one throwaway certificate.
const IDENTITY: &[u8] = include_bytes!("fake_imap.p12");
const IDENTITY_PASSWORD: &str = "mailypoppins";

/// One login the server was given, whatever it answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmtpLogin {
    pub username: String,
    pub password: String,
}

/// One message a DATA brought in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmtpDelivery {
    /// The reverse-path of `MAIL FROM`, without its brackets.
    pub from: String,
    /// The forward-path of every `RCPT TO` the server took, in order.
    pub recipients: Vec<String>,
    /// The message as the client meant it: CRLF line ends, the terminating
    /// `.` line gone and a leading dot of each line unstuffed.
    pub data: Vec<u8>,
    /// Whether the server answered the end of the data with a `2xx`: a
    /// message it stalled or hung up on was received all the same, and is
    /// what a client that got no verdict cannot tell from one delivered.
    pub acknowledged: bool,
}

/// One `RCPT TO` the server was given in a transaction, whatever it
/// answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmtpRcpt {
    /// The forward-path, without its brackets.
    pub address: String,
    /// The `4xx` or `5xx` reply it got, `None` when the server took it (or
    /// stalled or hung up on it).
    pub refusal: Option<String>,
}

/// The steps of a conversation a test can change the answer to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SmtpStep {
    /// The `220` a connection opens with.
    Greeting,
    /// `EHLO`, before STARTTLS and after it.
    Ehlo,
    /// `AUTH`, answered once the credentials are in.
    Auth,
    /// `MAIL FROM`.
    Mail,
    /// `RCPT TO`.
    Rcpt,
    /// `DATA`, before the message.
    Data,
    /// The end of the message, its `.` line.
    Message,
}

/// What the server does at a step in place of its success.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SmtpAnswer {
    /// This reply line, without its CRLF (`550 5.1.1 <x>: Recipient address
    /// rejected`).
    Reply(String),
    /// No answer: the server reads and discards whatever comes next until
    /// the client hangs up.
    Stall,
    /// The connection closed, no answer given.
    HangUp,
}

impl SmtpAnswer {
    /// [`SmtpAnswer::Reply`] of `line`.
    pub fn reply(line: &str) -> SmtpAnswer {
        SmtpAnswer::Reply(line.to_string())
    }
}

/// A step's answer for one recipient, keyed by the lowercase address and the
/// step, with how many more times it is given (`None`: every time).
type RecipientAnswers = HashMap<(String, SmtpStep), (SmtpAnswer, Option<usize>)>;

/// What every connection shares with the test.
#[derive(Default)]
struct Shared {
    logins: Mutex<Vec<SmtpLogin>>,
    deliveries: Mutex<Vec<SmtpDelivery>>,
    /// Every `RCPT TO` of a transaction, in order.
    rcpts: Mutex<Vec<SmtpRcpt>>,
    /// The answer of each step for every recipient.
    answers: Mutex<HashMap<SmtpStep, SmtpAnswer>>,
    /// The answer of a step of one recipient's transaction, keyed by the
    /// lowercase address, with how many more times it is given (`None`:
    /// every time).
    recipient_answers: Mutex<RecipientAnswers>,
    /// How many times the server came to each step.
    reached: Mutex<HashMap<SmtpStep, usize>>,
}

impl Shared {
    /// The answer at `step` for a transaction to `recipient`, if a test
    /// changed it, after counting the step reached.
    fn answer_at(&self, step: SmtpStep, recipient: Option<&str>) -> Option<SmtpAnswer> {
        *self
            .reached
            .lock()
            .expect("the step counts")
            .entry(step)
            .or_default() += 1;
        recipient
            .and_then(|address| {
                let mut answers = self
                    .recipient_answers
                    .lock()
                    .expect("the recipient answers");
                let key = (address.to_ascii_lowercase(), step);
                let (answer, left) = answers.get_mut(&key)?;
                let answer = answer.clone();
                if let Some(left) = left {
                    *left -= 1;
                    if *left == 0 {
                        answers.remove(&key);
                    }
                }
                Some(answer)
            })
            .or_else(|| {
                self.answers
                    .lock()
                    .expect("the answers")
                    .get(&step)
                    .cloned()
            })
    }
}

/// A running server, which lives as long as the test process.
pub struct RecordingSmtpServer {
    port: u16,
    shared: Arc<Shared>,
}

impl RecordingSmtpServer {
    /// Bind a free localhost port and serve it on a thread of its own, so a
    /// daemon in another process can reach it.
    pub fn start() -> RecordingSmtpServer {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a localhost port");
        let port = listener.local_addr().expect("its address").port();
        let shared = Arc::new(Shared::default());
        let served = Arc::clone(&shared);
        std::thread::spawn(move || {
            async_std::task::block_on(async move {
                let acceptor = async_native_tls::TlsAcceptor::new(IDENTITY, IDENTITY_PASSWORD)
                    .await
                    .expect("the fake's identity loads");
                let listener = TcpListener::from(listener);
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        continue;
                    };
                    let (acceptor, served) = (acceptor.clone(), Arc::clone(&served));
                    async_std::task::spawn(async move {
                        let _ = serve(stream, acceptor, served).await;
                    });
                }
            });
        });
        RecordingSmtpServer { port, shared }
    }

    /// The port an account's `[accounts.smtp]` names, with host `127.0.0.1`.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Every login the server was given so far, in order, whatever it
    /// answered.
    pub fn logins(&self) -> Vec<SmtpLogin> {
        self.shared.logins.lock().expect("the login list").clone()
    }

    /// Every message a DATA brought in so far, in order.
    pub fn deliveries(&self) -> Vec<SmtpDelivery> {
        self.shared
            .deliveries
            .lock()
            .expect("the delivery list")
            .clone()
    }

    /// Every `RCPT TO` of a transaction so far, in order, with the refusal
    /// it got: a recipient the server refused is in no delivery.
    pub fn rcpts(&self) -> Vec<SmtpRcpt> {
        self.shared.rcpts.lock().expect("the RCPT list").clone()
    }

    /// Answer every later `step` with `answer`, or as a server that takes
    /// it with `None`.
    pub fn answer(&self, step: SmtpStep, answer: Option<SmtpAnswer>) {
        let mut answers = self.shared.answers.lock().expect("the answers");
        match answer {
            Some(answer) => answers.insert(step, answer),
            None => answers.remove(&step),
        };
    }

    /// Answer `step` of every later transaction to `recipient` with
    /// `answer`, ahead of [`RecordingSmtpServer::answer`]. Only the steps
    /// that come once the recipient is named apply: [`SmtpStep::Rcpt`],
    /// [`SmtpStep::Data`] and [`SmtpStep::Message`].
    pub fn answer_for(&self, recipient: &str, step: SmtpStep, answer: Option<SmtpAnswer>) {
        let key = (recipient.to_ascii_lowercase(), step);
        let mut answers = self
            .shared
            .recipient_answers
            .lock()
            .expect("the recipient answers");
        match answer {
            Some(answer) => answers.insert(key, (answer, None)),
            None => answers.remove(&key),
        };
    }

    /// [`RecordingSmtpServer::answer_for`] for the next `times` transactions
    /// to `recipient` that reach `step`, after which the server takes them
    /// as before: a server that defers a recipient for a while and then
    /// comes back.
    pub fn answer_for_next(
        &self,
        recipient: &str,
        step: SmtpStep,
        answer: SmtpAnswer,
        times: usize,
    ) {
        if times == 0 {
            return;
        }
        self.shared
            .recipient_answers
            .lock()
            .expect("the recipient answers")
            .insert(
                (recipient.to_ascii_lowercase(), step),
                (answer, Some(times)),
            );
    }

    /// How many times the server came to `step` so far, whatever it
    /// answered.
    pub fn reached(&self, step: SmtpStep) -> usize {
        self.shared
            .reached
            .lock()
            .expect("the step counts")
            .get(&step)
            .copied()
            .unwrap_or(0)
    }
}

/// What a step of a connection comes to.
enum Next {
    /// Write this and read on.
    Write(String),
    /// Stall or hang up: the connection is over.
    Stop(SmtpAnswer),
}

/// The step's success, or what the test made of the step.
fn at(shared: &Shared, step: SmtpStep, recipient: Option<&str>, success: String) -> Next {
    match shared.answer_at(step, recipient) {
        None => Next::Write(success),
        Some(SmtpAnswer::Reply(line)) => Next::Write(format!("{line}\r\n")),
        Some(stop) => Next::Stop(stop),
    }
}

/// Whether a reply is a refusal, a `4xx` or a `5xx`.
fn refused(reply: &str) -> bool {
    matches!(reply.as_bytes().first(), Some(b'4' | b'5'))
}

/// The address of a `MAIL FROM:<a>` or `RCPT TO:<a>` line, without its
/// brackets or its parameters; a bracketed path ends at its `>`, so a quoted
/// local part with a space in it (`<"d "@example.org>`) stays whole.
fn path_of(line: &str) -> String {
    let rest = line.split_once(':').map_or("", |(_, rest)| rest).trim();
    if let Some((path, _)) = rest.strip_prefix('<').and_then(|r| r.split_once('>')) {
        return path.to_string();
    }
    let path = rest.split_whitespace().next().unwrap_or_default();
    path.trim_start_matches('<')
        .trim_end_matches('>')
        .to_string()
}

/// One connection: the greeting, EHLO and STARTTLS in plaintext, the TLS
/// upgrade, then EHLO again, AUTH and the mail transactions, until the
/// client quits or a step stops it.
async fn serve(
    mut plain: TcpStream,
    acceptor: async_native_tls::TlsAcceptor,
    shared: Arc<Shared>,
) -> std::io::Result<()> {
    match at(
        &shared,
        SmtpStep::Greeting,
        None,
        "220 localhost ESMTP fake\r\n".to_string(),
    ) {
        Next::Write(greeting) => {
            plain.write_all(greeting.as_bytes()).await?;
            if refused(&greeting) {
                // RFC 5321 3.1: a server that will not serve says so with a
                // 554 greeting and waits for QUIT.
                return refuse_all(&mut plain).await;
            }
        }
        Next::Stop(stop) => return stop_plain(&mut plain, &stop).await,
    }
    // Byte by byte until STARTTLS, so nothing of the TLS handshake that follows
    // is read into a buffer the acceptor never sees.
    loop {
        let Some(line) = read_plain_line(&mut plain).await? else {
            return Ok(());
        };
        let verb = verb_of(&line);
        let answer = match verb.as_str() {
            // RFC 3207 4: STARTTLS is offered, AUTH is not, before TLS.
            "EHLO" => at(
                &shared,
                SmtpStep::Ehlo,
                None,
                "250-localhost\r\n250-ENHANCEDSTATUSCODES\r\n250 STARTTLS\r\n".to_string(),
            ),
            "STARTTLS" => {
                plain.write_all(b"220 2.0.0 Ready to start TLS\r\n").await?;
                break;
            }
            "QUIT" => {
                plain.write_all(b"221 2.0.0 Bye\r\n").await?;
                return Ok(());
            }
            "NOOP" | "RSET" => Next::Write("250 2.0.0 Ok\r\n".to_string()),
            _ => Next::Write("530 5.7.0 Must issue a STARTTLS command first\r\n".to_string()),
        };
        match answer {
            Next::Write(reply) => plain.write_all(reply.as_bytes()).await?,
            Next::Stop(stop) => return stop_plain(&mut plain, &stop).await,
        }
    }
    let tls = acceptor
        .accept(plain)
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut tls = BufReader::new(tls);
    let mut authenticated = false;
    // The open transaction: its reverse-path and the recipients taken.
    let mut from: Option<String> = None;
    let mut recipients: Vec<String> = Vec::new();
    let mut queued = 0usize;
    loop {
        let Some(line) = read_tls_line(&mut tls).await? else {
            return Ok(());
        };
        let words: Vec<&str> = line.split_whitespace().collect();
        let verb = verb_of(&line);
        // The recipient a transaction-scoped answer is keyed by: the one
        // being named, or the first one taken.
        let named = (verb == "RCPT").then(|| path_of(&line));
        let recipient = named.as_deref().or(recipients.first().map(String::as_str));
        let answer = match verb.as_str() {
            // RFC 3207 4.2: the session starts over, STARTTLS is not offered
            // again, and AUTH now is.
            "EHLO" => at(
                &shared,
                SmtpStep::Ehlo,
                None,
                "250-localhost\r\n250-SIZE 10240000\r\n250-8BITMIME\r\n250-SMTPUTF8\r\n\
                 250-ENHANCEDSTATUSCODES\r\n250 AUTH PLAIN LOGIN\r\n"
                    .to_string(),
            ),
            "AUTH" => {
                let mechanism = words
                    .get(1)
                    .copied()
                    .unwrap_or_default()
                    .to_ascii_uppercase();
                let login = match mechanism.as_str() {
                    "PLAIN" => {
                        let initial = match words.get(2) {
                            Some(initial) => initial.to_string(),
                            None => {
                                reply(&mut tls, "334 \r\n").await?;
                                read_tls_line(&mut tls).await?.unwrap_or_default()
                            }
                        };
                        plain_login(&initial)
                    }
                    "LOGIN" => {
                        reply(&mut tls, "334 VXNlcm5hbWU6\r\n").await?;
                        let username = decode(&read_tls_line(&mut tls).await?.unwrap_or_default());
                        reply(&mut tls, "334 UGFzc3dvcmQ6\r\n").await?;
                        let password = decode(&read_tls_line(&mut tls).await?.unwrap_or_default());
                        Some(SmtpLogin { username, password })
                    }
                    _ => None,
                };
                match login {
                    Some(login) => {
                        shared.logins.lock().expect("the login list").push(login);
                        let answer = at(
                            &shared,
                            SmtpStep::Auth,
                            None,
                            "235 2.7.0 Authentication successful\r\n".to_string(),
                        );
                        authenticated = matches!(&answer, Next::Write(reply) if !refused(reply));
                        answer
                    }
                    None => {
                        Next::Write("504 5.5.4 Unrecognized authentication type\r\n".to_string())
                    }
                }
            }
            "MAIL" if !authenticated => {
                Next::Write("530 5.7.0 Authentication required\r\n".to_string())
            }
            "MAIL" if from.is_some() => {
                Next::Write("503 5.5.1 Error: nested MAIL command\r\n".to_string())
            }
            "MAIL" => {
                let answer = at(
                    &shared,
                    SmtpStep::Mail,
                    None,
                    "250 2.1.0 Ok\r\n".to_string(),
                );
                if matches!(&answer, Next::Write(reply) if !refused(reply)) {
                    from = Some(path_of(&line));
                }
                answer
            }
            "RCPT" if from.is_none() => {
                Next::Write("503 5.5.1 Error: need MAIL command\r\n".to_string())
            }
            "RCPT" => {
                let answer = at(
                    &shared,
                    SmtpStep::Rcpt,
                    recipient,
                    "250 2.1.5 Ok\r\n".to_string(),
                );
                if matches!(&answer, Next::Write(reply) if !refused(reply)) {
                    recipients.extend(named.clone());
                }
                shared.rcpts.lock().expect("the RCPT list").push(SmtpRcpt {
                    address: named.clone().unwrap_or_default(),
                    refusal: match &answer {
                        Next::Write(reply) if refused(reply) => Some(reply.trim_end().to_string()),
                        _ => None,
                    },
                });
                answer
            }
            "DATA" if recipients.is_empty() => {
                Next::Write("554 5.5.1 Error: no valid recipients\r\n".to_string())
            }
            "DATA" => {
                let answer = at(
                    &shared,
                    SmtpStep::Data,
                    recipient,
                    "354 End data with <CR><LF>.<CR><LF>\r\n".to_string(),
                );
                match answer {
                    Next::Write(go) if !refused(&go) => {
                        reply(&mut tls, &go).await?;
                        let Some(data) = read_data(&mut tls).await? else {
                            return Ok(());
                        };
                        queued += 1;
                        let answer = at(
                            &shared,
                            SmtpStep::Message,
                            recipient,
                            format!("250 2.0.0 Ok: queued as FAKE{queued}\r\n"),
                        );
                        shared
                            .deliveries
                            .lock()
                            .expect("the delivery list")
                            .push(SmtpDelivery {
                                from: from.take().unwrap_or_default(),
                                recipients: std::mem::take(&mut recipients),
                                data,
                                acknowledged: matches!(
                                    &answer,
                                    Next::Write(reply) if !refused(reply)
                                ),
                            });
                        answer
                    }
                    other => other,
                }
            }
            "RSET" => {
                from = None;
                recipients.clear();
                Next::Write("250 2.0.0 Ok\r\n".to_string())
            }
            "NOOP" => Next::Write("250 2.0.0 Ok\r\n".to_string()),
            "QUIT" => {
                reply(&mut tls, "221 2.0.0 Bye\r\n").await?;
                return Ok(());
            }
            _ => Next::Write("502 5.5.2 Error: command not recognized\r\n".to_string()),
        };
        match answer {
            Next::Write(answer) => reply(&mut tls, &answer).await?,
            Next::Stop(SmtpAnswer::Stall) => return drain(&mut tls).await,
            Next::Stop(_) => return Ok(()),
        }
    }
}

/// After a refusing greeting: every command but QUIT is refused, until the
/// client quits or hangs up.
async fn refuse_all(plain: &mut TcpStream) -> std::io::Result<()> {
    while let Some(line) = read_plain_line(plain).await? {
        if verb_of(&line) == "QUIT" {
            return plain.write_all(b"221 2.0.0 Bye\r\n").await;
        }
        plain.write_all(b"503 5.5.1 Error: no service\r\n").await?;
    }
    Ok(())
}

/// A stall or a hang-up before TLS.
async fn stop_plain(plain: &mut TcpStream, stop: &SmtpAnswer) -> std::io::Result<()> {
    if *stop == SmtpAnswer::Stall {
        while read_plain_line(plain).await?.is_some() {}
    }
    Ok(())
}

/// A stall after TLS: read and discard until the client hangs up.
async fn drain(tls: &mut BufReader<async_native_tls::TlsStream<TcpStream>>) -> std::io::Result<()> {
    let mut sink = Vec::new();
    while tls.read_until(b'\n', &mut sink).await? > 0 {
        sink.clear();
    }
    Ok(())
}

/// The message after a `354`, up to its `.` line, unstuffed; `None` once
/// the client hung up before the end.
async fn read_data(
    tls: &mut BufReader<async_native_tls::TlsStream<TcpStream>>,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut data = Vec::new();
    loop {
        let mut line = Vec::new();
        if tls.read_until(b'\n', &mut line).await? == 0 {
            return Ok(None);
        }
        if line == b".\r\n" {
            return Ok(Some(data));
        }
        // RFC 5321 4.5.2: the client doubled every leading dot.
        let unstuffed = line.strip_prefix(b".").unwrap_or(&line);
        data.extend_from_slice(unstuffed);
    }
}

/// The command verb of a line, upper case.
fn verb_of(line: &str) -> String {
    line.split([' ', '\r', '\n'])
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase()
}

/// One CRLF-terminated line off the plaintext stream, `None` once the client
/// hung up.
async fn read_plain_line(plain: &mut TcpStream) -> std::io::Result<Option<String>> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while !line.ends_with(b"\n") {
        if plain.read(&mut byte).await? == 0 {
            return Ok(None);
        }
        line.push(byte[0]);
    }
    Ok(Some(String::from_utf8_lossy(&line).into_owned()))
}

/// One line off the TLS stream without its line end, `None` once the client
/// hung up.
async fn read_tls_line(
    tls: &mut BufReader<async_native_tls::TlsStream<TcpStream>>,
) -> std::io::Result<Option<String>> {
    let mut line = Vec::new();
    if tls.read_until(b'\n', &mut line).await? == 0 {
        return Ok(None);
    }
    let line = String::from_utf8_lossy(&line);
    Ok(Some(line.trim_end_matches(['\r', '\n']).to_string()))
}

async fn reply(
    tls: &mut BufReader<async_native_tls::TlsStream<TcpStream>>,
    answer: &str,
) -> std::io::Result<()> {
    tls.get_mut().write_all(answer.as_bytes()).await?;
    tls.get_mut().flush().await
}

/// The base64 of one SASL step, as text.
fn decode(step: &str) -> String {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(step.trim())
        .unwrap_or_default();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// `authzid NUL authcid NUL password`, RFC 4616.
fn plain_login(initial: &str) -> Option<SmtpLogin> {
    let decoded = decode(initial);
    let mut parts = decoded.split('\0');
    let _authzid = parts.next()?;
    let username = parts.next()?.to_string();
    let password = parts.next()?.to_string();
    Some(SmtpLogin { username, password })
}
