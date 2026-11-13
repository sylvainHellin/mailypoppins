//! An SMTP server that records the credentials it is given (PERSO-199).
//!
//! `send::build_smtp_transport` is TLS-only: port 465 is implicit TLS and every
//! other port is STARTTLS. This server speaks STARTTLS on a free localhost
//! port with the throwaway identity [`super::fake_imap`] presents
//! (`fake_imap.p12`, CN `localhost`, SAN `127.0.0.1`), which an account reaches
//! with `accept_invalid_certs = true`, allowed for a loopback host only.
//!
//! It accepts every `AUTH PLAIN` and `AUTH LOGIN` and records the username and
//! password each one carried, which is what a test reads to prove the password
//! a client logged in with is the one stored. Past the login it answers `NOOP`,
//! `RSET`, `MAIL`, `RCPT` and `DATA` with success and keeps nothing of a
//! message: it is a login witness, not a mail sink.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use async_std::net::{TcpListener, TcpStream};
use base64::Engine;
use futures::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// The identity the server presents, and its export password: the fake IMAP
/// server's, so the repository carries one throwaway certificate.
const IDENTITY: &[u8] = include_bytes!("fake_imap.p12");
const IDENTITY_PASSWORD: &str = "mailypoppins";

/// One login the server accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmtpLogin {
    pub username: String,
    pub password: String,
}

/// A running server, which lives as long as the test process.
pub struct RecordingSmtpServer {
    port: u16,
    logins: Arc<Mutex<Vec<SmtpLogin>>>,
}

impl RecordingSmtpServer {
    /// Bind a free localhost port and serve it on a thread of its own, so a
    /// daemon in another process can reach it.
    pub fn start() -> RecordingSmtpServer {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a localhost port");
        let port = listener.local_addr().expect("its address").port();
        let logins = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&logins);
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
                    let (acceptor, recorded) = (acceptor.clone(), Arc::clone(&recorded));
                    async_std::task::spawn(async move {
                        let _ = serve(stream, acceptor, recorded).await;
                    });
                }
            });
        });
        RecordingSmtpServer { port, logins }
    }

    /// The port an account's `[accounts.smtp]` names, with host `127.0.0.1`.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Every login accepted so far, in order.
    pub fn logins(&self) -> Vec<SmtpLogin> {
        self.logins.lock().expect("the login list").clone()
    }
}

/// One connection: the plaintext greeting, EHLO and STARTTLS, the TLS upgrade,
/// then EHLO again, AUTH and whatever follows, until the client quits.
async fn serve(
    mut plain: TcpStream,
    acceptor: async_native_tls::TlsAcceptor,
    logins: Arc<Mutex<Vec<SmtpLogin>>>,
) -> std::io::Result<()> {
    plain.write_all(b"220 localhost fake ESMTP\r\n").await?;
    // Byte by byte until STARTTLS, so nothing of the TLS handshake that follows
    // is read into a buffer the acceptor never sees.
    loop {
        let Some(line) = read_plain_line(&mut plain).await? else {
            return Ok(());
        };
        let verb = line
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        match verb.as_str() {
            "EHLO" => {
                plain
                    .write_all(b"250-localhost\r\n250 STARTTLS\r\n")
                    .await?
            }
            "STARTTLS" => {
                plain.write_all(b"220 Ready to start TLS\r\n").await?;
                break;
            }
            "QUIT" => {
                plain.write_all(b"221 Bye\r\n").await?;
                return Ok(());
            }
            _ => {
                plain
                    .write_all(b"530 Must issue STARTTLS first\r\n")
                    .await?
            }
        }
    }
    let tls = acceptor
        .accept(plain)
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut tls = BufReader::new(tls);
    let mut in_data = false;
    loop {
        let mut line = String::new();
        if tls.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        if in_data {
            if line.trim_end_matches(['\r', '\n']) == "." {
                in_data = false;
                reply(&mut tls, "250 OK queued\r\n").await?;
            }
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        let verb = words
            .first()
            .copied()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let answer = match verb.as_str() {
            "EHLO" => "250-localhost\r\n250 AUTH PLAIN LOGIN\r\n".to_string(),
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
                                read_tls_line(&mut tls).await?
                            }
                        };
                        plain_login(&initial)
                    }
                    "LOGIN" => {
                        reply(&mut tls, "334 VXNlcm5hbWU6\r\n").await?;
                        let username = decode(&read_tls_line(&mut tls).await?);
                        reply(&mut tls, "334 UGFzc3dvcmQ6\r\n").await?;
                        let password = decode(&read_tls_line(&mut tls).await?);
                        Some(SmtpLogin { username, password })
                    }
                    _ => None,
                };
                match login {
                    Some(login) => {
                        logins.lock().expect("the login list").push(login);
                        "235 Authentication succeeded\r\n".to_string()
                    }
                    None => "504 Unrecognized authentication type\r\n".to_string(),
                }
            }
            "DATA" => {
                in_data = true;
                "354 End data with <CR><LF>.<CR><LF>\r\n".to_string()
            }
            "QUIT" => {
                reply(&mut tls, "221 Bye\r\n").await?;
                return Ok(());
            }
            _ => "250 OK\r\n".to_string(),
        };
        reply(&mut tls, &answer).await?;
    }
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

async fn read_tls_line(
    tls: &mut BufReader<async_native_tls::TlsStream<TcpStream>>,
) -> std::io::Result<String> {
    let mut line = String::new();
    tls.read_line(&mut line).await?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
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
