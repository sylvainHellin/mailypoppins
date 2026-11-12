//! An IMAP server that refuses logins, the way xmail.mwn.de refused a wrong
//! password for a day (PERSO-296).
//!
//! The client is TLS-only: port 993 is implicit TLS and every other port is
//! STARTTLS (`crate::imap_client::open_imap_session`), so a plaintext listener
//! never reaches LOGIN. This one speaks STARTTLS on a free localhost port with
//! a throwaway self-signed identity, `fake_imap.p12` beside this file (CN
//! `localhost`, SAN `127.0.0.1`, valid until 2126), which an account reaches
//! with `accept_invalid_certs = true`, allowed for a loopback host only.
//!
//! What it answers a LOGIN with is its [`LoginPolicy`], which a test switches
//! while the server runs: by default the TUM server's tagged
//! `NO [AUTHENTICATIONFAILED] LOGIN failed.`, or the temporary
//! `NO [UNAVAILABLE]` of RFC 5530, or Gmail's `NO [ALERT]` over its
//! connection limit, or `OK`. It counts every LOGIN. A login it
//! accepts gets an empty `INBOX` and nothing else: enough of a session for a
//! daemon tick to list, select, search and IDLE, so a test can let the
//! daemon in and then refuse it, the password changing while it runs.
//!
//! A test can replace that `INBOX` with folders of its own
//! ([`RejectingImapServer::set_folders`]): `LIST` names them, `STATUS`,
//! `SELECT` and `EXAMINE` report each one's UIDVALIDITY and UIDNEXT, and
//! `UID FETCH` of `BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)]` answers the
//! Message-ID of each listed UID the selected folder holds, which is what a
//! rename probe reads (PERSO-185). Nothing else reads the messages: a
//! `SEARCH` still finds none.
//!
//! Two test trees use it: the daemon suites through `mod support;`, and the
//! library's own unit tests, which include this file by path from
//! `src/imap_client/mod.rs`, so the pool's test and the health row drive the
//! same server.

// Each test tree uses a different part of it.
#![allow(dead_code)]

use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_std::net::{TcpListener, TcpStream};
use futures::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// The identity the server presents, and its export password.
const IDENTITY: &[u8] = include_bytes!("fake_imap.p12");
const IDENTITY_PASSWORD: &str = "mailypoppins";

/// The answer to a refused LOGIN, verbatim from the server of PERSO-296.
pub const LOGIN_FAILED: &str = "NO [AUTHENTICATIONFAILED] LOGIN failed.";

/// The answer of a server down for maintenance, RFC 5530's example.
pub const LOGIN_UNAVAILABLE: &str = "NO [UNAVAILABLE] User's backend down for maintenance";

/// Gmail's answer to a correct password over its connection limit.
pub const LOGIN_ALERT: &str = "NO [ALERT] Too many simultaneous connections. (Failure)";

/// What the server answers a LOGIN with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginPolicy {
    /// [`LOGIN_FAILED`], the password refused.
    Reject,
    /// [`LOGIN_UNAVAILABLE`], a passing state of the server.
    Unavailable,
    /// [`LOGIN_ALERT`], a passing state of the account.
    Alert,
    /// `OK`, and a session with an empty `INBOX`.
    Accept,
}

impl LoginPolicy {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => LoginPolicy::Unavailable,
            2 => LoginPolicy::Accept,
            3 => LoginPolicy::Alert,
            _ => LoginPolicy::Reject,
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            LoginPolicy::Reject => 0,
            LoginPolicy::Unavailable => 1,
            LoginPolicy::Accept => 2,
            LoginPolicy::Alert => 3,
        }
    }
}

/// One folder of the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FakeFolder {
    pub name: String,
    pub uidvalidity: u32,
    pub uidnext: u32,
    /// Each message's UID and its Message-ID, brackets included.
    pub messages: Vec<(u32, String)>,
}

impl FakeFolder {
    /// An empty folder with UIDVALIDITY 1, which is what `INBOX` is by
    /// default.
    pub fn empty(name: &str) -> FakeFolder {
        FakeFolder {
            name: name.to_string(),
            uidvalidity: 1,
            uidnext: 1,
            messages: Vec::new(),
        }
    }
}

/// What every connection shares with the test.
struct Shared {
    logins: AtomicUsize,
    policy: AtomicU8,
    /// The open connections, so a test can cut them all at once.
    connections: Mutex<Vec<TcpStream>>,
    /// What `LIST` names, in order.
    folders: Mutex<Vec<FakeFolder>>,
}

impl Shared {
    /// The folder `name`, or an empty one for a name `LIST` does not give,
    /// which is how this server answered every name before it held folders.
    fn folder(&self, name: &str) -> FakeFolder {
        self.folders
            .lock()
            .expect("the folder list")
            .iter()
            .find(|f| f.name == name)
            .cloned()
            .unwrap_or_else(|| FakeFolder::empty(name))
    }
}

/// A running server, which lives as long as the test process.
pub struct RejectingImapServer {
    port: u16,
    shared: Arc<Shared>,
}

impl RejectingImapServer {
    /// A server that refuses every login.
    pub fn start() -> RejectingImapServer {
        RejectingImapServer::start_with(LoginPolicy::Reject)
    }

    /// Bind a free localhost port and serve it on a thread of its own, so a
    /// test on any executor, or a daemon in another process, can reach it.
    pub fn start_with(policy: LoginPolicy) -> RejectingImapServer {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a localhost port");
        let port = listener.local_addr().expect("its address").port();
        let shared = Arc::new(Shared {
            logins: AtomicUsize::new(0),
            policy: AtomicU8::new(policy.as_u8()),
            connections: Mutex::new(Vec::new()),
            folders: Mutex::new(vec![FakeFolder::empty("INBOX")]),
        });
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
                    served
                        .connections
                        .lock()
                        .expect("the connection list")
                        .push(stream.clone());
                    let (acceptor, served) = (acceptor.clone(), Arc::clone(&served));
                    async_std::task::spawn(async move {
                        let _ = serve(stream, acceptor, served).await;
                    });
                }
            });
        });
        RejectingImapServer { port, shared }
    }

    /// The port an account's `[accounts.imap]` names, with host `127.0.0.1`.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// How many LOGIN commands the server has answered so far, whatever it
    /// answered.
    pub fn logins(&self) -> usize {
        self.shared.logins.load(Ordering::SeqCst)
    }

    /// Answer every later LOGIN with `policy`. Sessions already logged in
    /// stay logged in.
    pub fn set_policy(&self, policy: LoginPolicy) {
        self.shared.policy.store(policy.as_u8(), Ordering::SeqCst);
    }

    /// Serve `folders` in place of the empty `INBOX`, from the next command
    /// on.
    pub fn set_folders(&self, folders: Vec<FakeFolder>) {
        *self.shared.folders.lock().expect("the folder list") = folders;
    }

    /// Cut every open connection, the way a server drops the sessions of an
    /// account whose password changed: the client's next command on one of
    /// them fails, and it has to log in again.
    pub fn drop_sessions(&self) {
        for stream in self
            .shared
            .connections
            .lock()
            .expect("the connection list")
            .drain(..)
        {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// One connection: the plaintext greeting and STARTTLS, the TLS upgrade, then
/// the LOGIN policy and, once logged in, an empty `INBOX`, until the client
/// hangs up.
async fn serve(
    mut plain: TcpStream,
    acceptor: async_native_tls::TlsAcceptor,
    shared: Arc<Shared>,
) -> std::io::Result<()> {
    plain
        .write_all(b"* OK [CAPABILITY IMAP4rev1 STARTTLS] fake IMAP ready\r\n")
        .await?;
    // Byte by byte, so nothing of the TLS handshake that follows is read into
    // a buffer the acceptor never sees.
    let mut command = Vec::new();
    let mut byte = [0u8; 1];
    while !command.ends_with(b"\n") {
        if plain.read(&mut byte).await? == 0 {
            return Ok(());
        }
        command.push(byte[0]);
    }
    let command = String::from_utf8_lossy(&command);
    let tag = command.split_whitespace().next().unwrap_or("*");
    plain
        .write_all(format!("{tag} OK Begin TLS negotiation now\r\n").as_bytes())
        .await?;
    let tls = acceptor
        .accept(plain)
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    // The client injects the post-STARTTLS greeting itself, so the server
    // says nothing until it is asked.
    let mut tls = BufReader::new(tls);
    let mut logged_in = false;
    let mut selected: Option<FakeFolder> = None;
    loop {
        let mut line = String::new();
        if tls.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        let tag = words.first().copied().unwrap_or("*").to_string();
        let verb = words
            .get(1)
            .copied()
            .unwrap_or_default()
            .to_ascii_uppercase();
        // `UID SEARCH` answers like `SEARCH`, `UID FETCH` like `FETCH`.
        let command = if verb == "UID" {
            words
                .get(2)
                .copied()
                .unwrap_or_default()
                .to_ascii_uppercase()
        } else {
            verb.clone()
        };
        let reply = match command.as_str() {
            "LOGIN" => {
                shared.logins.fetch_add(1, Ordering::SeqCst);
                match LoginPolicy::from_u8(shared.policy.load(Ordering::SeqCst)) {
                    LoginPolicy::Reject => format!("{tag} {LOGIN_FAILED}\r\n"),
                    LoginPolicy::Unavailable => format!("{tag} {LOGIN_UNAVAILABLE}\r\n"),
                    LoginPolicy::Alert => format!("{tag} {LOGIN_ALERT}\r\n"),
                    LoginPolicy::Accept => {
                        logged_in = true;
                        format!("{tag} OK LOGIN completed\r\n")
                    }
                }
            }
            "CAPABILITY" => {
                format!("* CAPABILITY IMAP4rev1 IDLE\r\n{tag} OK CAPABILITY completed\r\n")
            }
            "LOGOUT" => format!("* BYE\r\n{tag} OK LOGOUT completed\r\n"),
            _ if !logged_in => format!("{tag} BAD not logged in\r\n"),
            "LIST" => {
                let mut reply = String::new();
                for folder in shared.folders.lock().expect("the folder list").iter() {
                    reply.push_str(&format!(
                        "* LIST (\\HasNoChildren) \"/\" \"{}\"\r\n",
                        folder.name
                    ));
                }
                format!("{reply}{tag} OK LIST completed\r\n")
            }
            "SELECT" | "EXAMINE" => {
                let folder = shared.folder(&quoted_name(&line));
                let reply = format!(
                    "* {} EXISTS\r\n* 0 RECENT\r\n* FLAGS (\\Seen \\Deleted \\Draft)\r\n\
                         * OK [UIDVALIDITY {}] UIDs valid\r\n\
                         * OK [UIDNEXT {}] Predicted next UID\r\n\
                         {tag} OK [READ-WRITE] {command} completed\r\n",
                    folder.messages.len(),
                    folder.uidvalidity,
                    folder.uidnext
                );
                selected = Some(folder);
                reply
            }
            "STATUS" => {
                let folder = shared.folder(&quoted_name(&line));
                format!(
                    "* STATUS \"{}\" (MESSAGES {} UNSEEN 0 UIDVALIDITY {} UIDNEXT {})\r\n\
                     {tag} OK STATUS completed\r\n",
                    folder.name,
                    folder.messages.len(),
                    folder.uidvalidity,
                    folder.uidnext
                )
            }
            "FETCH" if verb == "UID" => {
                let wanted = words.get(3).copied().unwrap_or_default();
                let mut reply = String::new();
                for (seq, (uid, id)) in selected.iter().flat_map(|f| f.messages.iter()).enumerate()
                {
                    if wanted.split(',').any(|w| w == uid.to_string()) {
                        let header = format!("Message-ID: {id}\r\n\r\n");
                        reply.push_str(&format!(
                            "* {} FETCH (UID {uid} BODY[HEADER.FIELDS (MESSAGE-ID)] {{{}}}\r\n{header})\r\n",
                            seq + 1,
                            header.len()
                        ));
                    }
                }
                format!("{reply}{tag} OK FETCH completed\r\n")
            }
            "SEARCH" => format!("* SEARCH\r\n{tag} OK SEARCH completed\r\n"),
            "IDLE" => {
                tls.get_mut().write_all(b"+ idling\r\n").await?;
                tls.get_mut().flush().await?;
                loop {
                    let mut done = String::new();
                    if tls.read_line(&mut done).await? == 0 {
                        return Ok(());
                    }
                    if done.trim().eq_ignore_ascii_case("DONE") {
                        break;
                    }
                }
                format!("{tag} OK IDLE terminated\r\n")
            }
            _ => format!("{tag} OK {command} completed\r\n"),
        };
        tls.get_mut().write_all(reply.as_bytes()).await?;
        tls.get_mut().flush().await?;
        if verb == "LOGOUT" {
            return Ok(());
        }
    }
}

/// The mailbox a `SELECT`, `EXAMINE` or `STATUS` line names: its first
/// quoted string, else its third word.
fn quoted_name(line: &str) -> String {
    let mut quoted = line.split('"');
    match (quoted.next(), quoted.next()) {
        (Some(_), Some(name)) => name.to_string(),
        _ => line
            .split_whitespace()
            .nth(2)
            .unwrap_or_default()
            .to_string(),
    }
}
