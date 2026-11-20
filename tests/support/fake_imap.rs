//! An IMAP server that refuses logins, the way xmail.mwn.de refused a wrong
//! password for a day (PERSO-296), and that holds folders a client can sync,
//! search and change (PERSO-300, PERSO-210).
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
//! connection limit, or a `BAD`, or no answer at all, the connection closed
//! (PERSO-307), or `OK`. It counts every LOGIN and keeps the password
//! each one carried ([`RejectingImapServer::passwords`]). A login it
//! accepts gets an empty `INBOX` and nothing else: enough of a session for a
//! daemon tick to list, select, search and IDLE, so a test can let the
//! daemon in and then refuse it, the password changing while it runs.
//!
//! A test can replace that `INBOX` with folders of its own
//! ([`RejectingImapServer::set_folders`]): `LIST` names them, `STATUS`,
//! `SELECT` and `EXAMINE` report each one's UIDVALIDITY and UIDNEXT, and
//! `UID FETCH` of `BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)]` answers the
//! Message-ID of each listed UID the selected folder holds, which is what a
//! rename probe reads (PERSO-185). Such a folder has no bodies: a `SEARCH`
//! or a `FLAGS` fetch of it finds none, which is the one place this server
//! answers as no real one would.
//!
//! A folder given whole messages ([`RejectingImapServer::set_mail`]) is one a
//! client can sync and change, and every session sees the same folder
//! (PERSO-210): `UID SEARCH` evaluates its criteria against the messages,
//! `UID FETCH` answers `UID`, `FLAGS`, `BODY.PEEK[]` and
//! `BODY.PEEK[HEADER.FIELDS (...)]`, `UID STORE` changes the flags,
//! `UID COPY` files a copy under the destination's next UID, `EXPUNGE` and
//! `UID EXPUNGE` remove what is flagged `\Deleted` and `APPEND` takes a
//! message in. The server offers `UIDPLUS` (RFC 4315) unless a test turns it
//! off ([`RejectingImapServer::set_uidplus`]), and a test reads what a
//! folder holds back ([`RejectingImapServer::contents`]). A test can slow
//! every body download down ([`RejectingImapServer::set_body_delay`]) and
//! count them ([`RejectingImapServer::body_fetches`]), and read back every
//! `SEARCH` it ran ([`RejectingImapServer::searches`]).
//!
//! A test can make the next APPENDs file their message and hang up before
//! the answer ([`RejectingImapServer::hang_up_after_appends`]), or answer
//! late ([`RejectingImapServer::set_append_delay`]), and can deliver a
//! message to a folder while a session idles on it
//! ([`RejectingImapServer::deliver`]), which that session is told of with an
//! untagged `EXISTS` (RFC 2177 3); [`RejectingImapServer::idles`] counts the
//! IDLEs entered, so a test delivers once the client is listening.
//!
//! A test can make the server refuse any command with a tagged answer of its
//! own ([`RejectingImapServer::refuse`]), and answer a `SEARCH` the way TUM's
//! Exchange does (PERSO-214): one under `CHARSET UTF-8` with
//! `NO [BADCHARSET (US-ASCII)]`, before the literal
//! ([`RejectingImapServer::refuse_utf8_search`]).
//!
//! Sequence numbers are positions in the shared folder, and a session is
//! told of no change another session made (no unsolicited `EXISTS` or
//! `EXPUNGE`, RFC 3501 7.4.1): the client addresses messages by UID only, so
//! nothing it does depends on that.
//!
//! Two test trees use it: the daemon suites through `mod support;`, and the
//! library's own unit tests, which include this file by path from
//! `src/imap_client/mod.rs`, so the pool's test and the health row drive the
//! same server.

// Each test tree uses a different part of it.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
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

/// A `BAD` to LOGIN, which says nothing about the password.
pub const LOGIN_BAD: &str = "BAD LOGIN command garbled";

/// What [`RejectingImapServer::refuse_utf8_search`] answers a `SEARCH` under a
/// `CHARSET` with, after the tag.
pub const SEARCH_BADCHARSET: &str =
    "NO [BADCHARSET (US-ASCII)] The specified charset is not supported.";

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
    /// No answer: the server closes the connection once the LOGIN came, as
    /// a server that drops a bad password's connection does (PERSO-307).
    Hangup,
    /// [`LOGIN_BAD`], an answer that is neither `OK` nor `NO`.
    Bad,
}

impl LoginPolicy {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => LoginPolicy::Unavailable,
            2 => LoginPolicy::Accept,
            3 => LoginPolicy::Alert,
            4 => LoginPolicy::Hangup,
            5 => LoginPolicy::Bad,
            _ => LoginPolicy::Reject,
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            LoginPolicy::Reject => 0,
            LoginPolicy::Unavailable => 1,
            LoginPolicy::Accept => 2,
            LoginPolicy::Alert => 3,
            LoginPolicy::Hangup => 4,
            LoginPolicy::Bad => 5,
        }
    }
}

/// One folder of the server, as a test gives it and as
/// [`RejectingImapServer::set_folders`] serves it.
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

/// One whole message of a folder a sync runs against: its UID and its bytes.
pub type FakeMessage = (u32, Vec<u8>);

/// One message as the server holds it now, for a test to read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldMessage {
    pub uid: u32,
    /// The `Message-ID` header's value, brackets included, or the one the
    /// folder was given.
    pub message_id: String,
    /// The flags, system flags with their backslash, in the order set.
    pub flags: Vec<String>,
    /// The bytes, `None` for a message [`RejectingImapServer::set_folders`]
    /// gave only a Message-ID.
    pub raw: Option<Vec<u8>>,
}

/// One folder as the server holds it.
#[derive(Clone, Debug)]
struct Mailbox {
    name: String,
    uidvalidity: u32,
    uidnext: u32,
    /// By ascending UID, so a message's sequence number is its position
    /// plus one (RFC 3501 2.3.1.2).
    messages: Vec<HeldMessage>,
}

impl Mailbox {
    fn empty(name: &str) -> Mailbox {
        Mailbox {
            name: name.to_string(),
            uidvalidity: 1,
            uidnext: 1,
            messages: Vec::new(),
        }
    }

    /// The highest UID held, which `*` stands for in a UID set (RFC 3501
    /// 6.4.8), 0 for an empty folder.
    fn max_uid(&self) -> u32 {
        self.messages.last().map_or(0, |m| m.uid)
    }

    /// Take `raw` in under the next UID, as APPEND and COPY do.
    fn take(&mut self, message_id: String, flags: Vec<String>, raw: Option<Vec<u8>>) -> u32 {
        let uid = self.uidnext;
        self.uidnext += 1;
        self.messages.push(HeldMessage {
            uid,
            message_id,
            flags,
            raw,
        });
        uid
    }
}

/// What every connection shares with the test.
struct Shared {
    logins: AtomicUsize,
    /// The password of every LOGIN, in order, whatever it was answered.
    passwords: Mutex<Vec<String>>,
    policy: AtomicU8,
    /// The open connections, so a test can cut them all at once.
    connections: Mutex<Vec<TcpStream>>,
    /// Every folder, in the order `LIST` names them.
    mailboxes: Mutex<Vec<Mailbox>>,
    /// How long the server waits before it answers a `BODY.PEEK[]` fetch, in
    /// milliseconds.
    body_delay_ms: AtomicU64,
    /// How many `BODY.PEEK[]` fetches the server has answered.
    body_fetches: AtomicUsize,
    /// How long the server waits before it answers a LOGIN, in milliseconds,
    /// as a server delays a refused login.
    login_delay_ms: AtomicU64,
    /// Whether a `SEARCH` under a `CHARSET` is answered `NO [BADCHARSET]`.
    refuse_utf8_search: AtomicBool,
    /// The tagged answer a command gets in place of its work, by verb
    /// (`STORE`, `COPY`, `APPEND`, `SEARCH`, ...).
    refusals: Mutex<HashMap<String, String>>,
    /// Whether CAPABILITY offers UIDPLUS.
    uidplus: AtomicBool,
    /// How many of the next APPENDs file the message and then close the
    /// connection instead of answering it.
    append_hangups: AtomicUsize,
    /// How long the server waits after it filed an APPEND's message before
    /// it answers, in milliseconds.
    append_delay_ms: AtomicU64,
    /// The sessions in IDLE, each with the folder it has selected and the
    /// channel that wakes it to report what arrived there.
    idlers: Mutex<Vec<(String, futures::channel::mpsc::UnboundedSender<()>)>>,
    /// How many IDLE commands the server has entered.
    idles: AtomicUsize,
    /// The messages, by UID, a FETCH leaves out, and the tagged answer it
    /// ends with when it named one of them.
    fetch_refusals: Mutex<HashMap<u32, String>>,
    /// The tagged answer, with no data, to a FETCH of bodies naming more
    /// than one message, when set.
    chunk_refusal: Mutex<Option<String>>,
    /// The verbs whose command closes the connection, after an untagged
    /// line and before the tagged answer.
    hangups: Mutex<std::collections::HashSet<String>>,
    /// The folder and the command line of every `SEARCH` the server ran.
    searches: Mutex<Vec<(String, String)>>,
}

impl Shared {
    /// The folder `name` as it is now, or an empty one for a name `LIST`
    /// does not give, which is how this server answered every name before
    /// it held folders.
    fn mailbox(&self, name: &str) -> Mailbox {
        self.mailboxes
            .lock()
            .expect("the folders")
            .iter()
            .find(|m| m.name == name)
            .cloned()
            .unwrap_or_else(|| Mailbox::empty(name))
    }

    /// Run `change` on the folder `name`, `None` when the server has none
    /// of that name.
    fn change<T>(&self, name: &str, change: impl FnOnce(&mut Mailbox) -> T) -> Option<T> {
        self.mailboxes
            .lock()
            .expect("the folders")
            .iter_mut()
            .find(|m| m.name == name)
            .map(change)
    }

    fn refusal(&self, verb: &str) -> Option<String> {
        self.refusals
            .lock()
            .expect("the refusals")
            .get(verb)
            .cloned()
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
            passwords: Mutex::new(Vec::new()),
            policy: AtomicU8::new(policy.as_u8()),
            connections: Mutex::new(Vec::new()),
            mailboxes: Mutex::new(vec![Mailbox::empty("INBOX")]),
            body_delay_ms: AtomicU64::new(0),
            body_fetches: AtomicUsize::new(0),
            login_delay_ms: AtomicU64::new(0),
            refuse_utf8_search: AtomicBool::new(false),
            refusals: Mutex::new(HashMap::new()),
            uidplus: AtomicBool::new(true),
            append_hangups: AtomicUsize::new(0),
            append_delay_ms: AtomicU64::new(0),
            idlers: Mutex::new(Vec::new()),
            idles: AtomicUsize::new(0),
            fetch_refusals: Mutex::new(HashMap::new()),
            chunk_refusal: Mutex::new(None),
            hangups: Mutex::new(std::collections::HashSet::new()),
            searches: Mutex::new(Vec::new()),
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

    /// The password of every LOGIN so far, in order, whatever the server
    /// answered: what a test reads to prove which stored password a client
    /// logged in with (PERSO-199).
    pub fn passwords(&self) -> Vec<String> {
        self.shared
            .passwords
            .lock()
            .expect("the password list")
            .clone()
    }

    /// Answer every later LOGIN with `policy`. Sessions already logged in
    /// stay logged in.
    pub fn set_policy(&self, policy: LoginPolicy) {
        self.shared.policy.store(policy.as_u8(), Ordering::SeqCst);
    }

    /// Serve `folders` in place of every folder, from the next command on.
    /// Their messages have a Message-ID and no bytes.
    pub fn set_folders(&self, folders: Vec<FakeFolder>) {
        *self.shared.mailboxes.lock().expect("the folders") = folders
            .into_iter()
            .map(|folder| Mailbox {
                name: folder.name,
                uidvalidity: folder.uidvalidity,
                uidnext: folder.uidnext,
                messages: folder
                    .messages
                    .into_iter()
                    .map(|(uid, message_id)| HeldMessage {
                        uid,
                        message_id,
                        // Seen, so a folder of Message-IDs counts no unread
                        // mail, as it did before it held any.
                        flags: vec!["\\Seen".to_string()],
                        raw: None,
                    })
                    .collect(),
            })
            .collect();
    }

    /// Serve `name` with these whole messages, by ascending UID, no flag
    /// set, under `uidvalidity`, in place of any folder of that name, from
    /// the next command on: what a sync downloads (PERSO-300).
    pub fn set_mail(&self, name: &str, uidvalidity: u32, messages: Vec<FakeMessage>) {
        let mut sorted = messages;
        sorted.sort_by_key(|(uid, _)| *uid);
        let mailbox = Mailbox {
            name: name.to_string(),
            uidvalidity,
            uidnext: sorted.iter().map(|(uid, _)| uid + 1).max().unwrap_or(1),
            messages: sorted
                .into_iter()
                .map(|(uid, raw)| HeldMessage {
                    uid,
                    message_id: message_id_of(&raw)
                        .unwrap_or_else(|| format!("<{uid}@{name}.fake>")),
                    flags: Vec::new(),
                    raw: Some(raw),
                })
                .collect(),
        };
        let mut mailboxes = self.shared.mailboxes.lock().expect("the folders");
        match mailboxes.iter_mut().find(|m| m.name == name) {
            Some(held) => *held = mailbox,
            None => mailboxes.push(mailbox),
        }
    }

    /// Stop serving the folder `name`, as a server whose folder another
    /// client deleted: `LIST` no longer names it.
    pub fn remove_folder(&self, name: &str) {
        self.shared
            .mailboxes
            .lock()
            .expect("the folders")
            .retain(|mailbox| mailbox.name != name);
    }

    /// Set the flags of the message `uid` of `name`, as another client
    /// would.
    pub fn set_flags(&self, name: &str, uid: u32, flags: &[&str]) {
        self.shared
            .change(name, |mailbox| {
                for message in mailbox.messages.iter_mut().filter(|m| m.uid == uid) {
                    message.flags = flags.iter().map(|f| f.to_string()).collect();
                }
            })
            .expect("the folder is served");
    }

    /// What the folder `name` holds now, by ascending UID, empty for a
    /// folder the server has not got.
    pub fn contents(&self, name: &str) -> Vec<HeldMessage> {
        self.shared.mailbox(name).messages
    }

    /// Wait `delay` before answering each later `BODY.PEEK[]` fetch: a slow
    /// download, for a test of the body budget (PERSO-300).
    pub fn set_body_delay(&self, delay: std::time::Duration) {
        let millis = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
        self.shared.body_delay_ms.store(millis, Ordering::SeqCst);
    }

    /// Wait `delay` before answering each later LOGIN, as a server delays a
    /// refused login by seconds: a login in flight that long, for a test of
    /// the dials racing it (PERSO-296).
    pub fn set_login_delay(&self, delay: std::time::Duration) {
        let millis = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
        self.shared.login_delay_ms.store(millis, Ordering::SeqCst);
    }

    /// Answer each later `SEARCH` that names a `CHARSET` with
    /// [`SEARCH_BADCHARSET`], as TUM's Exchange answers `CHARSET UTF-8`
    /// (PERSO-214); a search in plain ASCII is answered as before.
    pub fn refuse_utf8_search(&self, refuse: bool) {
        self.shared
            .refuse_utf8_search
            .store(refuse, Ordering::SeqCst);
    }

    /// Answer every later `SEARCH` with `answer`, the tagged status and text
    /// after the tag (`NO [UNAVAILABLE] search is down`), or as before with
    /// `None`.
    pub fn refuse_searches(&self, answer: Option<&str>) {
        self.refuse("SEARCH", answer);
    }

    /// Answer every later command `verb` (`STORE`, `COPY`, `EXPUNGE`,
    /// `APPEND`, `SELECT`, ..., a `UID` command by the verb after `UID`)
    /// with `answer`, the tagged status and text after the tag, changing
    /// nothing; or serve it again with `None`. An `APPEND` is refused before
    /// its literal, as a server refuses a mailbox it will not take into.
    pub fn refuse(&self, verb: &str, answer: Option<&str>) {
        let mut refusals = self.shared.refusals.lock().expect("the refusals");
        let verb = verb.to_ascii_uppercase();
        match answer {
            Some(answer) => refusals.insert(verb, answer.to_string()),
            None => refusals.remove(&verb),
        };
    }

    /// Close the connection at every later command `verb` once logged in,
    /// after an untagged `* OK` and before its tagged answer, as a server
    /// that goes away mid-answer does; or answer it again with `false`.
    pub fn hang_up_on(&self, verb: &str, on: bool) {
        let mut hangups = self.shared.hangups.lock().expect("the hang-ups");
        let verb = verb.to_ascii_uppercase();
        if on {
            hangups.insert(verb);
        } else {
            hangups.remove(&verb);
        }
    }

    /// Offer UIDPLUS (RFC 4315) or not, from the next CAPABILITY on: the
    /// default is to offer it, as every server mp has met does.
    pub fn set_uidplus(&self, offered: bool) {
        self.shared.uidplus.store(offered, Ordering::SeqCst);
    }

    /// File the message of each of the next `count` APPENDs and then close
    /// the connection without the tagged answer: the copy is on the server
    /// and the client cannot know it, which is the ambiguity the outbox's
    /// dedup search exists for (#0116).
    pub fn hang_up_after_appends(&self, count: usize) {
        self.shared.append_hangups.store(count, Ordering::SeqCst);
    }

    /// Wait `delay` between filing each later APPEND's message and
    /// answering it: the window in which a second drain could append the
    /// same row again.
    pub fn set_append_delay(&self, delay: std::time::Duration) {
        let millis = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
        self.shared.append_delay_ms.store(millis, Ordering::SeqCst);
    }

    /// A message `raw` arrives in `folder` under its next UID, unseen, as
    /// mail delivered by another server does, and every session idling on
    /// that folder is told (RFC 2177 3, an untagged `EXISTS`). Answers the
    /// new UID.
    pub fn deliver(&self, folder: &str, raw: Vec<u8>) -> u32 {
        let message_id = message_id_of(&raw).unwrap_or_default();
        let uid = self
            .shared
            .change(folder, |mailbox| {
                mailbox.take(message_id, Vec::new(), Some(raw))
            })
            .expect("the folder is served");
        for (idling, wake) in self.shared.idlers.lock().expect("the idlers").iter() {
            if idling == folder {
                let _ = wake.unbounded_send(());
            }
        }
        uid
    }

    /// Leave the message `uid` out of every later FETCH of bodies that names
    /// it, its flags and headers still answered, and end that FETCH with
    /// `answer`, the tagged status and text after the
    /// tag, after the messages it did send; or fetch it again with `None`.
    /// A server answers `NO` with partial data so (RFC 2180 4.1.2), Gmail's
    /// `NO Some messages could not be FETCHed (Failure)` among them.
    pub fn refuse_fetch_of(&self, uid: u32, answer: Option<&str>) {
        let mut refusals = self.shared.fetch_refusals.lock().expect("the refusals");
        match answer {
            Some(answer) => refusals.insert(uid, answer.to_string()),
            None => refusals.remove(&uid),
        };
    }

    /// Answer every later FETCH of bodies that names more than one message
    /// with `answer` and no data at all, a FETCH of one message's body as
    /// before; or every FETCH as before with `None`. An outage-like refusal
    /// that names no message, which only a FETCH of one can pin on it.
    pub fn refuse_body_chunks(&self, answer: Option<&str>) {
        *self.shared.chunk_refusal.lock().expect("the refusal") = answer.map(str::to_string);
    }

    /// A message `raw` arrives in `folder` under its next UID, unseen, and no
    /// session idling there is told: mail a test syncs itself, which the
    /// daemon's own watcher, idling on INBOX, must not sync first. Answers
    /// the new UID.
    pub fn deliver_unannounced(&self, folder: &str, raw: Vec<u8>) -> u32 {
        let message_id = message_id_of(&raw).unwrap_or_default();
        self.shared
            .change(folder, |mailbox| {
                mailbox.take(message_id, Vec::new(), Some(raw))
            })
            .expect("the folder is served")
    }

    /// How many IDLE commands the server has entered so far: what a test
    /// waits on before it delivers, so the arrival is one IDLE reports.
    pub fn idles(&self) -> usize {
        self.shared.idles.load(Ordering::SeqCst)
    }

    /// How many `BODY.PEEK[]` fetches the server has answered so far.
    pub fn body_fetches(&self) -> usize {
        self.shared.body_fetches.load(Ordering::SeqCst)
    }

    /// The folder and the command line of every `SEARCH` the server ran so
    /// far, in order, a refused one left out: what a test reads to prove a
    /// client looked before it acted (the outbox's dedup search, #0116).
    pub fn searches(&self) -> Vec<(String, String)> {
        self.shared
            .searches
            .lock()
            .expect("the search list")
            .clone()
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

/// One argument of a command line.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    /// An atom, a sequence set or a fetch item, brackets and all
    /// (`BODY.PEEK[HEADER.FIELDS (DATE)]`).
    Atom(String),
    /// A quoted string or a literal, its bytes.
    Str(Vec<u8>),
    /// A parenthesised list.
    List(Vec<Token>),
}

impl Token {
    /// The atom or the string as text.
    fn text(&self) -> String {
        match self {
            Token::Atom(atom) => atom.clone(),
            Token::Str(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            Token::List(_) => String::new(),
        }
    }
}

/// The tokens of one line, and the size of the literal it ends with, if it
/// ends with one (`{12}`, RFC 3501 4.3).
fn tokenize(line: &[u8]) -> (Vec<Token>, Option<usize>) {
    let line = line
        .strip_suffix(b"\r\n")
        .or_else(|| line.strip_suffix(b"\n"))
        .unwrap_or(line);
    let mut literal = None;
    let mut body = line;
    if line.ends_with(b"}") {
        if let Some(open) = line.iter().rposition(|&b| b == b'{') {
            if let Some(size) = std::str::from_utf8(&line[open + 1..line.len() - 1])
                .ok()
                .and_then(|n| n.trim_end_matches('+').parse().ok())
            {
                literal = Some(size);
                body = &line[..open];
            }
        }
    }
    let mut stack: Vec<Vec<Token>> = vec![Vec::new()];
    let mut i = 0;
    while i < body.len() {
        match body[i] {
            b' ' => i += 1,
            b'(' => {
                stack.push(Vec::new());
                i += 1;
            }
            b')' => {
                let list = stack.pop().unwrap_or_default();
                stack
                    .last_mut()
                    .expect("the outer list")
                    .push(Token::List(list));
                i += 1;
            }
            b'"' => {
                let mut text = Vec::new();
                i += 1;
                while i < body.len() && body[i] != b'"' {
                    if body[i] == b'\\' && i + 1 < body.len() {
                        i += 1;
                    }
                    text.push(body[i]);
                    i += 1;
                }
                i += 1;
                stack.last_mut().expect("a list").push(Token::Str(text));
            }
            _ => {
                let start = i;
                let mut depth = 0;
                while i < body.len() {
                    match body[i] {
                        b'[' => depth += 1,
                        b']' => depth -= 1,
                        b' ' | b'(' | b')' if depth == 0 => break,
                        _ => {}
                    }
                    i += 1;
                }
                let atom = String::from_utf8_lossy(&body[start..i]).into_owned();
                stack.last_mut().expect("a list").push(Token::Atom(atom));
            }
        }
    }
    while stack.len() > 1 {
        let list = stack.pop().unwrap_or_default();
        stack
            .last_mut()
            .expect("the outer list")
            .push(Token::List(list));
    }
    (stack.pop().unwrap_or_default(), literal)
}

/// The Message-ID header of `raw`, if it has one.
fn message_id_of(raw: &[u8]) -> Option<String> {
    header_fields(raw, &["MESSAGE-ID"])
        .split_once(':')
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The header section of `raw`, without the blank line that ends it.
fn header_section(raw: &[u8]) -> &[u8] {
    let end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|at| at + 2)
        .unwrap_or(raw.len());
    &raw[..end]
}

/// The lines of the header fields `names` (upper case) of `raw`, folded
/// lines included, as `HEADER.FIELDS` returns them (RFC 3501 6.4.5), without
/// the blank line.
fn header_fields(raw: &[u8], names: &[&str]) -> String {
    let headers = String::from_utf8_lossy(header_section(raw)).into_owned();
    let mut out = String::new();
    let mut keep = false;
    for line in headers.split_inclusive("\r\n") {
        if !line.starts_with([' ', '\t']) {
            let name = line.split(':').next().unwrap_or_default().trim();
            keep = names.iter().any(|n| n.eq_ignore_ascii_case(name));
        }
        if keep {
            out.push_str(line);
        }
    }
    out
}

/// The unfolded value of the header field `name` of `raw`, every instance
/// joined, empty when it has none.
fn header_value(raw: &[u8], name: &str) -> String {
    header_fields(raw, &[name])
        .split("\r\n")
        .map(|line| {
            line.split_once(':')
                .filter(|(field, _)| field.eq_ignore_ascii_case(name))
                .map_or(line, |(_, value)| value)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether `haystack` contains `needle`, ignoring case, as SEARCH matches
/// strings (RFC 3501 6.4.4).
fn contains(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// Whether `message` matches the search keys `keys`, all of them (RFC 3501
/// 6.4.4), or why the server cannot tell: a key it does not speak.
fn matches(message: &HeldMessage, keys: &[Token], max: u32) -> Result<bool, String> {
    let raw = message.raw.as_deref().unwrap_or_default();
    let flagged = |flag: &str| message.flags.iter().any(|f| f.eq_ignore_ascii_case(flag));
    let mut keys = keys.iter();
    let mut all = true;
    while let Some(key) = keys.next() {
        let mut arg = || keys.next().map(Token::text).unwrap_or_default();
        let hit = match key {
            Token::List(inner) => matches(message, inner, max)?,
            Token::Atom(atom) => match atom.to_ascii_uppercase().as_str() {
                "ALL" => true,
                "SEEN" => flagged("\\Seen"),
                "UNSEEN" => !flagged("\\Seen"),
                "DELETED" => flagged("\\Deleted"),
                "UNDELETED" => !flagged("\\Deleted"),
                "FLAGGED" => flagged("\\Flagged"),
                "UNFLAGGED" => !flagged("\\Flagged"),
                "ANSWERED" => flagged("\\Answered"),
                "UNANSWERED" => !flagged("\\Answered"),
                "DRAFT" => flagged("\\Draft"),
                "UNDRAFT" => !flagged("\\Draft"),
                "UID" => in_set(&arg(), message.uid, max),
                "HEADER" => {
                    let field = arg();
                    let wanted = arg();
                    let value = header_value(raw, &field);
                    // RFC 3501 6.4.4: an empty string matches every message
                    // that has the field.
                    !header_fields(raw, &[&field]).is_empty() && contains(&value, &wanted)
                }
                field @ ("SUBJECT" | "FROM" | "TO" | "CC" | "BCC") => {
                    contains(&header_value(raw, field), &arg())
                }
                "BODY" => {
                    let body = &raw[header_section(raw).len()..];
                    contains(&String::from_utf8_lossy(body), &arg())
                }
                "TEXT" => contains(&String::from_utf8_lossy(raw), &arg()),
                "NOT" => {
                    let inner = keys.next().cloned().into_iter().collect::<Vec<_>>();
                    // A key with arguments under NOT is not one this
                    // server takes apart.
                    !matches(message, &inner, max)?
                }
                other => return Err(format!("the fake does not speak the search key {other}")),
            },
            Token::Str(_) => return Err("a search key cannot be a string".to_string()),
        };
        all &= hit;
    }
    Ok(all)
}

/// The fetch items of a FETCH line, rendered for one message, `UID` first:
/// a `UID FETCH` answers the UID whether it was asked or not (RFC 3501
/// 6.4.8).
fn fetch_items(message: &HeldMessage, items: &str) -> Vec<u8> {
    let upper = items.to_ascii_uppercase();
    let mut out = format!("UID {}", message.uid).into_bytes();
    if upper.contains("FLAGS") {
        out.extend_from_slice(format!(" FLAGS ({})", message.flags.join(" ")).as_bytes());
    }
    if upper.contains("RFC822.SIZE") {
        let size = message.raw.as_ref().map_or(0, Vec::len);
        out.extend_from_slice(format!(" RFC822.SIZE {size}").as_bytes());
    }
    if let Some(start) = upper.find("HEADER.FIELDS (") {
        let rest = &upper[start + "HEADER.FIELDS (".len()..];
        let names: Vec<&str> = rest
            .split(')')
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .collect();
        let header = match &message.raw {
            Some(raw) => header_fields(raw, &names),
            None => format!("Message-ID: {}\r\n", message.message_id),
        };
        // RFC 3501 6.4.5: the fields and the blank line after them.
        let header = format!("{header}\r\n");
        out.extend_from_slice(
            format!(
                " BODY[HEADER.FIELDS ({})] {{{}}}\r\n",
                names.join(" "),
                header.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(header.as_bytes());
    } else if upper.contains("BODY.PEEK[]") || upper.contains("BODY[]") || upper.contains("RFC822)")
    {
        let raw = message.raw.clone().unwrap_or_default();
        out.extend_from_slice(format!(" BODY[] {{{}}}\r\n", raw.len()).as_bytes());
        out.extend_from_slice(&raw);
    }
    out
}

/// One connection: the plaintext greeting and STARTTLS, the TLS upgrade, then
/// the LOGIN policy and, once logged in, the folders, until the client hangs
/// up.
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
    // The selected folder's name, and whether it was opened read-only.
    let mut selected: Option<(String, bool)> = None;
    loop {
        let mut first = Vec::new();
        if tls.read_until(b'\n', &mut first).await? == 0 {
            return Ok(());
        }
        let line = String::from_utf8_lossy(&first).into_owned();
        let (mut tokens, mut literal) = tokenize(&first);
        let tag = tokens
            .first()
            .map(Token::text)
            .unwrap_or_else(|| "*".into());
        let verb = tokens
            .get(1)
            .map(Token::text)
            .unwrap_or_default()
            .to_ascii_uppercase();
        let uid = verb == "UID";
        // `UID SEARCH` answers like `SEARCH`, `UID FETCH` like `FETCH`.
        let command = if uid {
            tokens
                .get(2)
                .map(Token::text)
                .unwrap_or_default()
                .to_ascii_uppercase()
        } else {
            verb.clone()
        };
        // A command refused before its literal is answered at once, and the
        // client sends no literal (RFC 3501 7.5).
        let early = if !logged_in || command == "LOGIN" {
            None
        } else if let Some(answer) = shared.refusal(&command) {
            Some(answer)
        } else if command == "SEARCH"
            && shared.refuse_utf8_search.load(Ordering::SeqCst)
            && line.to_ascii_uppercase().contains(" CHARSET ")
        {
            Some(SEARCH_BADCHARSET.to_string())
        } else {
            None
        };
        if logged_in
            && shared
                .hangups
                .lock()
                .expect("the hang-ups")
                .contains(&command)
        {
            reply(&mut tls, b"* OK going away\r\n").await?;
            return tls.get_ref().get_ref().shutdown(std::net::Shutdown::Both);
        }
        if let Some(answer) = early {
            reply(&mut tls, format!("{tag} {answer}\r\n").as_bytes()).await?;
            continue;
        }
        while let Some(size) = literal.take() {
            reply(&mut tls, b"+ Ready for literal data\r\n").await?;
            let mut bytes = vec![0u8; size];
            tls.read_exact(&mut bytes).await?;
            tokens.push(Token::Str(bytes));
            let mut rest = Vec::new();
            tls.read_until(b'\n', &mut rest).await?;
            let (more, next) = tokenize(&rest);
            tokens.extend(more);
            literal = next;
        }
        let args: &[Token] = tokens.get(if uid { 3 } else { 2 }..).unwrap_or_default();
        let answer: Vec<u8> = match command.as_str() {
            "LOGIN" => {
                shared.logins.fetch_add(1, Ordering::SeqCst);
                let delay = shared.login_delay_ms.load(Ordering::SeqCst);
                if delay > 0 {
                    async_std::task::sleep(std::time::Duration::from_millis(delay)).await;
                }
                shared
                    .passwords
                    .lock()
                    .expect("the password list")
                    .push(login_password(&line));
                match LoginPolicy::from_u8(shared.policy.load(Ordering::SeqCst)) {
                    LoginPolicy::Reject => format!("{tag} {LOGIN_FAILED}\r\n"),
                    LoginPolicy::Unavailable => format!("{tag} {LOGIN_UNAVAILABLE}\r\n"),
                    LoginPolicy::Alert => format!("{tag} {LOGIN_ALERT}\r\n"),
                    LoginPolicy::Bad => format!("{tag} {LOGIN_BAD}\r\n"),
                    LoginPolicy::Hangup => {
                        // The connection list keeps a clone of the socket,
                        // so dropping this end would leave it open.
                        return tls.get_ref().get_ref().shutdown(std::net::Shutdown::Both);
                    }
                    LoginPolicy::Accept => {
                        logged_in = true;
                        format!("{tag} OK LOGIN completed\r\n")
                    }
                }
                .into_bytes()
            }
            "CAPABILITY" => {
                let uidplus = if shared.uidplus.load(Ordering::SeqCst) {
                    " UIDPLUS"
                } else {
                    ""
                };
                format!("* CAPABILITY IMAP4rev1 IDLE{uidplus}\r\n{tag} OK CAPABILITY completed\r\n")
                    .into_bytes()
            }
            "LOGOUT" => format!("* BYE\r\n{tag} OK LOGOUT completed\r\n").into_bytes(),
            _ if !logged_in => format!("{tag} BAD not logged in\r\n").into_bytes(),
            "LIST" => {
                let mut reply = String::new();
                for mailbox in shared.mailboxes.lock().expect("the folders").iter() {
                    reply.push_str(&format!(
                        "* LIST (\\HasNoChildren) \"/\" \"{}\"\r\n",
                        mailbox.name
                    ));
                }
                format!("{reply}{tag} OK LIST completed\r\n").into_bytes()
            }
            "SELECT" | "EXAMINE" => {
                let name = args.first().map(Token::text).unwrap_or_default();
                let mailbox = shared.mailbox(&name);
                // RFC 3501 6.3.2: EXAMINE opens the folder read-only.
                let read_only = command == "EXAMINE";
                let access = if read_only { "READ-ONLY" } else { "READ-WRITE" };
                selected = Some((name, read_only));
                format!(
                    "* {} EXISTS\r\n* 0 RECENT\r\n\
                     * FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)\r\n\
                     * OK [PERMANENTFLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft \\*)] Limited\r\n\
                     * OK [UIDVALIDITY {}] UIDs valid\r\n\
                     * OK [UIDNEXT {}] Predicted next UID\r\n\
                     {tag} OK [{access}] {command} completed\r\n",
                    mailbox.messages.len(),
                    mailbox.uidvalidity,
                    mailbox.uidnext
                )
                .into_bytes()
            }
            "STATUS" => {
                let mailbox = shared.mailbox(&args.first().map(Token::text).unwrap_or_default());
                let unseen = mailbox
                    .messages
                    .iter()
                    .filter(|m| !m.flags.iter().any(|f| f == "\\Seen"))
                    .count();
                format!(
                    "* STATUS \"{}\" (MESSAGES {} UNSEEN {unseen} UIDVALIDITY {} UIDNEXT {})\r\n\
                     {tag} OK STATUS completed\r\n",
                    mailbox.name,
                    mailbox.messages.len(),
                    mailbox.uidvalidity,
                    mailbox.uidnext
                )
                .into_bytes()
            }
            "APPEND" => {
                let answer = append(&shared, &tag, args, selected.as_ref());
                let delay = shared.append_delay_ms.load(Ordering::SeqCst);
                if delay > 0 {
                    async_std::task::sleep(std::time::Duration::from_millis(delay)).await;
                }
                let filed = !answer.starts_with(format!("{tag} NO").as_bytes());
                let hang_up = shared
                    .append_hangups
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok();
                if filed && hang_up {
                    return tls.get_ref().get_ref().shutdown(std::net::Shutdown::Both);
                }
                answer
            }
            "IDLE" => {
                use futures::{FutureExt, StreamExt};
                let (wake, mut woken) = futures::channel::mpsc::unbounded();
                let folder = selected.as_ref().map(|(name, _)| name.clone());
                if let Some(folder) = &folder {
                    shared
                        .idlers
                        .lock()
                        .expect("the idlers")
                        .push((folder.clone(), wake));
                }
                reply(&mut tls, b"+ idling\r\n").await?;
                shared.idles.fetch_add(1, Ordering::SeqCst);
                let ended = loop {
                    let mut done = String::new();
                    // The client sends nothing but DONE while it idles, so a
                    // read dropped for a wake loses nothing.
                    let event = futures::select! {
                        read = tls.read_line(&mut done).fuse() => Some(read),
                        _ = woken.next() => None,
                    };
                    match event {
                        Some(Ok(0)) | Some(Err(_)) => break false,
                        Some(Ok(_)) if done.trim().eq_ignore_ascii_case("DONE") => break true,
                        Some(Ok(_)) => {}
                        None => {
                            // RFC 2177 3: the new size of the idling folder.
                            let exists = folder
                                .as_deref()
                                .map_or(0, |name| shared.mailbox(name).messages.len());
                            reply(&mut tls, format!("* {exists} EXISTS\r\n").as_bytes()).await?;
                        }
                    }
                };
                drop(woken);
                shared
                    .idlers
                    .lock()
                    .expect("the idlers")
                    .retain(|(_, wake)| !wake.is_closed());
                if !ended {
                    return Ok(());
                }
                format!("{tag} OK IDLE terminated\r\n").into_bytes()
            }
            "FETCH" | "SEARCH" | "STORE" | "COPY" | "EXPUNGE" | "CLOSE" => {
                let Some((name, read_only)) = selected.clone() else {
                    let answer = format!("{tag} BAD No mailbox selected\r\n");
                    reply(&mut tls, answer.as_bytes()).await?;
                    continue;
                };
                if command == "FETCH" && line.to_ascii_uppercase().contains("BODY.PEEK[]") {
                    let delay = shared.body_delay_ms.load(Ordering::SeqCst);
                    if delay > 0 {
                        async_std::task::sleep(std::time::Duration::from_millis(delay)).await;
                    }
                    shared.body_fetches.fetch_add(1, Ordering::SeqCst);
                }
                if command == "SEARCH" {
                    shared
                        .searches
                        .lock()
                        .expect("the search list")
                        .push((name.clone(), line.clone()));
                }
                on_selected(
                    &shared,
                    &Command {
                        tag: &tag,
                        verb: &command,
                        uid,
                        args,
                        line: &line,
                    },
                    &name,
                    read_only,
                )
            }
            _ => format!("{tag} OK {command} completed\r\n").into_bytes(),
        };
        reply(&mut tls, &answer).await?;
        if command == "CLOSE" {
            selected = None;
        }
        if verb == "LOGOUT" {
            return Ok(());
        }
    }
}

async fn reply(
    tls: &mut BufReader<async_native_tls::TlsStream<TcpStream>>,
    bytes: &[u8],
) -> std::io::Result<()> {
    tls.get_mut().write_all(bytes).await?;
    tls.get_mut().flush().await
}

/// `APPEND mailbox [(flags)] [date] literal`: the message under the
/// folder's next UID, and its `APPENDUID` (RFC 4315 3); a folder the server
/// has not got is `NO [TRYCREATE]` (RFC 3501 6.3.11).
fn append(
    shared: &Shared,
    tag: &str,
    args: &[Token],
    selected: Option<&(String, bool)>,
) -> Vec<u8> {
    let name = args.first().map(Token::text).unwrap_or_default();
    let flags: Vec<String> = args
        .iter()
        .find_map(|t| match t {
            Token::List(flags) => Some(flags.iter().map(Token::text).collect()),
            _ => None,
        })
        .unwrap_or_default();
    let Some(Token::Str(raw)) = args.last().cloned() else {
        return format!("{tag} BAD APPEND has no message\r\n").into_bytes();
    };
    let uidplus = shared.uidplus.load(Ordering::SeqCst);
    let message_id = message_id_of(&raw).unwrap_or_default();
    let taken = shared.change(&name, |mailbox| {
        let uid = mailbox.take(message_id, flags, Some(raw));
        (mailbox.uidvalidity, uid, mailbox.messages.len())
    });
    let Some((uidvalidity, uid, exists)) = taken else {
        return format!("{tag} NO [TRYCREATE] Mailbox doesn't exist: {name}\r\n").into_bytes();
    };
    // RFC 3501 6.3.11: a session with the folder selected is told it grew.
    let exists = match selected {
        Some((open, _)) if *open == name => format!("* {exists} EXISTS\r\n"),
        _ => String::new(),
    };
    let code = if uidplus {
        format!("[APPENDUID {uidvalidity} {uid}] ")
    } else {
        String::new()
    };
    format!("{exists}{tag} OK {code}APPEND completed\r\n").into_bytes()
}

/// One command line as the client sent it.
struct Command<'a> {
    tag: &'a str,
    /// The verb, upper case, the one after `UID` for a UID command.
    verb: &'a str,
    /// Whether it came as `UID <verb>`, its numbers UIDs.
    uid: bool,
    /// The arguments after the verb.
    args: &'a [Token],
    /// The whole line, for what the tokens do not keep apart.
    line: &'a str,
}

/// The commands that act on the selected folder `name`, opened read-only
/// when `read_only`.
fn on_selected(shared: &Shared, cmd: &Command<'_>, name: &str, read_only: bool) -> Vec<u8> {
    let Command {
        tag,
        verb: command,
        uid,
        args,
        line,
    } = *cmd;
    let mailbox = shared.mailbox(name);
    let max = if uid {
        mailbox.max_uid()
    } else {
        u32::try_from(mailbox.messages.len()).unwrap_or(u32::MAX)
    };
    // Whether the message at `seq` is named by the set, by UID or by
    // sequence number.
    let named = |set: &str, seq: usize, message: &HeldMessage| {
        let number = if uid {
            message.uid
        } else {
            u32::try_from(seq).unwrap_or(u32::MAX)
        };
        in_set(set, number, max)
    };
    let set = args.first().map(Token::text).unwrap_or_default();
    let ok = |what: &str| format!("{tag} OK {what} completed\r\n").into_bytes();
    let changes = matches!(command, "STORE" | "EXPUNGE");
    if read_only && changes {
        // RFC 3501 6.3.2: nothing changes a folder opened with EXAMINE.
        return format!("{tag} NO [READ-ONLY] Mailbox is read-only\r\n").into_bytes();
    }
    match command {
        "FETCH" => {
            let mut out = Vec::new();
            let refusals = shared.fetch_refusals.lock().expect("the refusals").clone();
            let upper = line.to_ascii_uppercase();
            let bodies = upper.contains("BODY.PEEK[]") || upper.contains("BODY[]");
            let chunk_refusal = shared.chunk_refusal.lock().expect("the refusal").clone();
            if let Some(answer) = chunk_refusal.filter(|_| bodies) {
                let named_count = mailbox
                    .messages
                    .iter()
                    .enumerate()
                    .filter(|(index, message)| named(&set, index + 1, message))
                    .count();
                if named_count > 1 {
                    return format!("{tag} {answer}\r\n").into_bytes();
                }
            }
            let mut refused = None;
            for (index, message) in mailbox.messages.iter().enumerate() {
                let header_only = line.to_ascii_uppercase().contains("HEADER.FIELDS");
                // A folder of Message-IDs has no flags or bodies to fetch.
                if !named(&set, index + 1, message) || (message.raw.is_none() && !header_only) {
                    continue;
                }
                // The message is there and its flags and headers read; it is
                // its body the server cannot produce.
                let body = line.to_ascii_uppercase().contains("BODY.PEEK[]")
                    || line.to_ascii_uppercase().contains("BODY[]");
                if let Some(answer) = refusals.get(&message.uid).filter(|_| body) {
                    refused = Some(answer.clone());
                    continue;
                }
                out.extend_from_slice(format!("* {} FETCH (", index + 1).as_bytes());
                out.extend_from_slice(&fetch_items(message, line));
                out.extend_from_slice(b")\r\n");
            }
            match refused {
                Some(answer) => out.extend_from_slice(format!("{tag} {answer}\r\n").as_bytes()),
                None => out.extend_from_slice(&ok("FETCH")),
            }
            out
        }
        "SEARCH" => {
            let mut keys = args;
            if keys
                .first()
                .is_some_and(|k| k.text().eq_ignore_ascii_case("CHARSET"))
            {
                keys = keys.get(2..).unwrap_or_default();
            }
            let mut hits = String::new();
            for (index, message) in mailbox.messages.iter().enumerate() {
                if message.raw.is_none() {
                    continue;
                }
                match matches(message, keys, mailbox.max_uid()) {
                    Ok(true) => {
                        let number = if uid { message.uid as usize } else { index + 1 };
                        hits.push_str(&format!(" {number}"));
                    }
                    Ok(false) => {}
                    Err(why) => return format!("{tag} BAD [CLIENTBUG] {why}\r\n").into_bytes(),
                }
            }
            [format!("* SEARCH{hits}\r\n").into_bytes(), ok("SEARCH")].concat()
        }
        "STORE" => {
            let change = args
                .get(1)
                .map(Token::text)
                .unwrap_or_default()
                .to_ascii_uppercase();
            let flags: Vec<String> = match args.get(2) {
                Some(Token::List(flags)) => flags.iter().map(Token::text).collect(),
                Some(one) => vec![one.text()],
                None => Vec::new(),
            };
            let silent = change.ends_with(".SILENT");
            let mut out = Vec::new();
            shared.change(name, |held| {
                for (index, message) in held.messages.iter_mut().enumerate() {
                    if !named(&set, index + 1, message) {
                        continue;
                    }
                    let has = |held: &[String], flag: &str| {
                        held.iter().any(|f| f.eq_ignore_ascii_case(flag))
                    };
                    if change.starts_with('+') {
                        for flag in &flags {
                            if !has(&message.flags, flag) {
                                message.flags.push(flag.clone());
                            }
                        }
                    } else if change.starts_with('-') {
                        message.flags.retain(|f| !has(&flags, f));
                    } else {
                        message.flags = flags.clone();
                    }
                    // RFC 3501 6.4.6: the new flags, unless .SILENT, and a
                    // UID STORE's answer carries the UID (6.4.8).
                    if !silent {
                        out.extend_from_slice(
                            format!(
                                "* {} FETCH (UID {} FLAGS ({}))\r\n",
                                index + 1,
                                message.uid,
                                message.flags.join(" ")
                            )
                            .as_bytes(),
                        );
                    }
                }
            });
            out.extend_from_slice(&ok("STORE"));
            out
        }
        "COPY" => {
            let destination = args.get(1).map(Token::text).unwrap_or_default();
            let copies: Vec<HeldMessage> = mailbox
                .messages
                .iter()
                .enumerate()
                .filter(|(index, message)| named(&set, index + 1, message))
                .map(|(_, message)| message.clone())
                .collect();
            // RFC 3501 6.4.7: the flags go with the copy.
            let filed = shared.change(&destination, |dest| {
                let uids: Vec<u32> = copies
                    .iter()
                    .map(|m| dest.take(m.message_id.clone(), m.flags.clone(), m.raw.clone()))
                    .collect();
                (dest.uidvalidity, uids)
            });
            let Some((uidvalidity, new)) = filed else {
                // RFC 3501 6.4.7: a destination that does not exist.
                return format!("{tag} NO [TRYCREATE] Mailbox doesn't exist: {destination}\r\n")
                    .into_bytes();
            };
            let list = |uids: Vec<u32>| {
                uids.iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            };
            if new.is_empty() || !shared.uidplus.load(Ordering::SeqCst) {
                return ok("COPY");
            }
            // RFC 4315 3: COPYUID names the source and destination UIDs in
            // the same order.
            let old = list(copies.iter().map(|m| m.uid).collect());
            format!(
                "{tag} OK [COPYUID {uidvalidity} {old} {}] COPY completed\r\n",
                list(new)
            )
            .into_bytes()
        }
        "EXPUNGE" | "CLOSE" => {
            // RFC 4315 2.1: UID EXPUNGE removes only the named messages
            // flagged \Deleted; EXPUNGE and CLOSE (RFC 3501 6.4.2) every one.
            let mut out = Vec::new();
            if !read_only {
                shared.change(name, |held| {
                    let mut index = 0;
                    while index < held.messages.len() {
                        let message = &held.messages[index];
                        let deleted = message.flags.iter().any(|f| f == "\\Deleted");
                        let in_scope = !uid || in_set(&set, message.uid, held.max_uid());
                        if deleted && in_scope {
                            held.messages.remove(index);
                            // RFC 3501 7.4.1: each EXPUNGE renumbers the
                            // messages after it at once, so the next one
                            // removed may carry the same number.
                            if command == "EXPUNGE" {
                                out.extend_from_slice(
                                    format!("* {} EXPUNGE\r\n", index + 1).as_bytes(),
                                );
                            }
                        } else {
                            index += 1;
                        }
                    }
                });
            }
            out.extend_from_slice(&ok(command));
            out
        }
        _ => ok(command),
    }
}

/// Whether a sequence set (`3`, `1:20`, `5:*`, `1,4:6`) names `number`, `*`
/// standing for `max`, the highest number in use: a range ending in `*`
/// always names the last message, even past it (RFC 3501 6.4.8).
fn in_set(set: &str, number: u32, max: u32) -> bool {
    let bound = |end: &str| {
        if end == "*" {
            max
        } else {
            end.parse::<u32>().unwrap_or(u32::MAX)
        }
    };
    set.split(',').any(|part| match part.split_once(':') {
        Some((low, high)) => {
            let (low, high) = (bound(low), bound(high));
            (low.min(high)..=low.max(high)).contains(&number)
        }
        None => bound(part) == number,
    })
}

/// The password of a `LOGIN "user" "password"` line, which `async-imap`
/// writes as two quoted strings with `\` and `"` escaped by a backslash.
fn login_password(line: &str) -> String {
    let mut quoted = Vec::new();
    let mut current: Option<String> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (&mut current, c) {
            (None, '"') => current = Some(String::new()),
            (Some(_), '"') => quoted.extend(current.take()),
            (Some(s), '\\') => s.extend(chars.next()),
            (Some(s), c) => s.push(c),
            (None, _) => {}
        }
    }
    quoted.get(1).cloned().unwrap_or_default()
}
