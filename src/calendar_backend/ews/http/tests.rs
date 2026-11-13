//! The transport against a scripted HTTP server on the loopback: each
//! connection the server accepts answers its requests from a script, and
//! every request is recorded with the connection it came on.
//!
//! The server's type 2 is the spike's captured challenge
//! (`tests/fixtures/ews/ntlm_type2.hex`).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;

use super::*;
use crate::calendar_backend::ews::fetch::{EwsFetch, EwsFetchError};
use crate::config::ServerLogin;
use crate::login_gate::{self, Login};
use mp_protocol::account::RejectedLogin;

const TYPE2: &str = include_str!("../../../../tests/fixtures/ews/ntlm_type2.hex");

const ENVELOPE: &str = "<s:Envelope>a &amp; b</s:Envelope>";

fn hex(text: &str) -> Vec<u8> {
    let digits: String = text.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    (0..digits.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&digits[i..i + 2], 16).unwrap())
        .collect()
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// One request the server read: the connection it came on, its request
/// line, its headers and its body.
#[derive(Debug, Clone)]
struct Request {
    connection: usize,
    line: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The NTLM message the `Authorization` header carries.
    fn ntlm(&self) -> Option<Vec<u8>> {
        let token = self.header("Authorization")?.strip_prefix("NTLM ")?;
        base64::engine::general_purpose::STANDARD.decode(token).ok()
    }

    /// The NTLM message type the request carries, 0 for none.
    fn ntlm_type(&self) -> u32 {
        self.ntlm()
            .map(|msg| u32::from_le_bytes(msg[8..12].try_into().unwrap()))
            .unwrap_or(0)
    }
}

/// What the server does after one answer on a connection.
#[derive(Clone)]
enum Then {
    /// Read the next request.
    Next,
    /// Close the connection.
    Close,
}

/// One answer: the raw response, then what follows. `None` reads the
/// request and never answers.
type Step = (Option<String>, Then);

/// The server: the script of each connection in the order they come; a
/// connection past the script is counted and closed at once.
struct Server {
    port: u16,
    requests: Arc<Mutex<Vec<Request>>>,
    connections: Arc<AtomicUsize>,
}

impl Server {
    fn start(script: Vec<Vec<Step>>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let (seen, count) = (Arc::clone(&requests), Arc::clone(&connections));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let index = count.fetch_add(1, Ordering::SeqCst);
                let Some(steps) = script.get(index).cloned() else {
                    continue;
                };
                let seen = Arc::clone(&seen);
                std::thread::spawn(move || serve(index, stream, steps, &seen));
            }
        });
        Server {
            port,
            requests,
            connections,
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/EWS/Exchange.asmx", self.port)
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    /// How many connections the client opened, once any late one had time
    /// to arrive.
    fn connections(&self) -> usize {
        std::thread::sleep(Duration::from_millis(100));
        self.connections.load(Ordering::SeqCst)
    }
}

fn serve(index: usize, stream: TcpStream, steps: Vec<Step>, seen: &Mutex<Vec<Request>>) {
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    for (answer, then) in steps {
        let Some(request) = read_request(index, &mut reader) else {
            return;
        };
        seen.lock().unwrap().push(request);
        let Some(answer) = answer else {
            std::thread::sleep(Duration::from_secs(5));
            return;
        };
        if writer.write_all(answer.as_bytes()).is_err() {
            return;
        }
        if let Then::Close = then {
            let _ = writer.shutdown(std::net::Shutdown::Both);
            return;
        }
    }
}

fn read_request(connection: usize, reader: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut request = read_head(connection, reader)?;
    let length: usize = request
        .header("Content-Length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    request.body = String::from_utf8(body).ok()?;
    Some(request)
}

/// The request line and the headers of the next request, its body unread.
fn read_head(connection: usize, reader: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (key, value) = header.split_once(':')?;
        headers.push((key.trim().to_string(), value.trim().to_string()));
    }
    Some(Request {
        connection,
        line: line.trim_end().to_string(),
        headers,
        body: String::new(),
    })
}

/// A response with `status`, `headers` and a `Content-Length` body.
fn response(status: &str, headers: &[&str], body: &str) -> String {
    let mut text = format!("HTTP/1.1 {status}\r\n");
    for header in headers {
        text.push_str(header);
        text.push_str("\r\n");
    }
    text.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    text
}

/// The 401 that answers a type 1 with the server's challenge, as IIS sends
/// it: Negotiate and NTLM offered in two headers, the challenge on the
/// second.
fn challenge(extra: &[&str]) -> String {
    let ntlm = format!("WWW-Authenticate: NTLM {}", b64(&hex(TYPE2)));
    let mut headers = vec!["WWW-Authenticate: Negotiate", ntlm.as_str()];
    headers.extend_from_slice(extra);
    response("401 Unauthorized", &headers, "Unauthorized")
}

/// The 401 of a login the server refused, or of a request without one.
fn refused() -> String {
    response(
        "401 Unauthorized",
        &["WWW-Authenticate: Negotiate", "WWW-Authenticate: NTLM"],
        "Unauthorized",
    )
}

fn ok(body: &str) -> String {
    response("200 OK", &["Content-Type: text/xml; charset=utf-8"], body)
}

/// A 200 whose body comes in chunks.
fn ok_chunked(parts: &[&str]) -> String {
    let mut text = String::from(
        "HTTP/1.1 200 OK\r\nContent-Type: text/xml; charset=utf-8\r\n\
         Transfer-Encoding: chunked\r\n\r\n",
    );
    for part in parts {
        text.push_str(&format!("{:x};ext=1\r\n{part}\r\n", part.len()));
    }
    text.push_str("0\r\n\r\n");
    text
}

/// A user name no other test logs in as: the gate is one list for the whole
/// process, and a refusal one test records would hold another's login.
fn fresh_user(what: &str) -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    format!("ews-{what}-{}", NEXT.fetch_add(1, Ordering::SeqCst))
}

fn timeouts(request: Duration) -> Timeouts {
    Timeouts {
        connect: Duration::from_secs(2),
        request,
        login: Duration::from_secs(5),
    }
}

/// A transport to `server` as `user` with `password`.
fn transport_as(server: &Server, user: &str, password: &str, timeouts: Timeouts) -> EwsHttp {
    EwsHttp::plain_for_test(&server.url(), user, password, timeouts)
}

/// A transport to `server` as a user of its own.
fn transport(server: &Server, request: Duration) -> EwsHttp {
    transport_as(server, &fresh_user("call"), "secret", timeouts(request))
}

/// The gate's IMAP login of `user`, on another host, as the TUM's IMAP
/// login presents the user its EWS login does.
fn imap_login(user: &str) -> Login {
    Login::Imap(ServerLogin {
        account: format!("imap-of-{user}"),
        host: "imap.example.test".into(),
        port: 993,
        username: user.into(),
    })
}

/// The happy path: the type 1 and the type 3 go on one connection, each
/// with the whole envelope, the server's answer to the type 3 is the call's,
/// and the next call goes on the same connection with no login, its chunked
/// answer read whole.
#[tokio::test]
async fn the_login_and_the_next_call_go_on_one_connection() {
    let server = Server::start(vec![vec![
        (Some(challenge(&[])), Then::Next),
        (Some(ok("<first/>")), Then::Next),
        (Some(ok_chunked(&["<sec", "ond/>"])), Then::Next),
    ]]);
    let user = fresh_user("happy");
    let http = transport_as(&server, &user, "secret", timeouts(Duration::from_secs(5)));
    assert_eq!(http.post(ENVELOPE).await.unwrap(), "<first/>");
    assert_eq!(http.post(ENVELOPE).await.unwrap(), "<second/>");
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| request.connection == 0));
    assert_eq!(
        requests.iter().map(Request::ntlm_type).collect::<Vec<_>>(),
        [1, 3, 0]
    );
    assert_eq!(requests[0].ntlm().unwrap(), ntlm::negotiate_message());
    let type3 = requests[1].ntlm().unwrap();
    let user: Vec<u8> = user
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    assert!(type3.windows(user.len()).any(|window| window == user));
    for request in &requests {
        assert_eq!(request.line, "POST /EWS/Exchange.asmx HTTP/1.1");
        assert_eq!(request.body, ENVELOPE);
        assert_eq!(
            request.header("Content-Type"),
            Some("text/xml; charset=utf-8")
        );
        assert_eq!(
            request.header("Host"),
            Some(format!("127.0.0.1:{}", server.port).as_str())
        );
        assert_eq!(request.header("Connection"), Some("keep-alive"));
    }
    assert_eq!(server.connections(), 1);
}

/// A 401 after the type 3 is a refused login: one type 3 is sent, the call
/// is [`EwsFetchError::LoginRefused`], and every later call of the
/// transport answers the same refusal without reaching the server.
#[tokio::test]
async fn a_401_after_the_type_3_is_a_refused_login_never_asked_again() {
    let server = Server::start(vec![vec![
        (Some(challenge(&[])), Then::Next),
        (Some(refused()), Then::Next),
    ]]);
    let http = transport(&server, Duration::from_secs(5));
    let err = http.post(ENVELOPE).await.unwrap_err();
    assert!(
        matches!(err, EwsFetchError::LoginRefused(ref why)
            if why.starts_with("EWS login failed: HTTP 401")),
        "{err:?}"
    );
    let again = http.post(ENVELOPE).await.unwrap_err();
    assert!(matches!(again, EwsFetchError::LoginRefused(_)), "{again:?}");
    let requests = server.requests();
    assert_eq!(
        requests.iter().map(Request::ntlm_type).collect::<Vec<_>>(),
        [1, 3]
    );
    assert_eq!(server.connections(), 1, "nothing asked after the refusal");
}

/// A server that closes the connection after its challenge leaves the type
/// 3 nowhere to go: no type 3 is sent on another connection, and the call is
/// a transport failure, never a refused login.
#[tokio::test]
async fn a_server_that_closes_after_the_challenge_gets_no_type_3_elsewhere() {
    // Said in the answer, and done without a word.
    for extra in [&["Connection: close"][..], &[]] {
        let server = Server::start(vec![vec![(Some(challenge(extra)), Then::Close)]]);
        let http = transport(&server, Duration::from_secs(5));
        let err = http.post(ENVELOPE).await.unwrap_err();
        assert!(matches!(err, EwsFetchError::Transport(_)), "{err:?}");
        assert!(err.to_string().contains("closed the connection"), "{err}");
        let requests = server.requests();
        assert_eq!(requests.len(), 1, "{extra:?}");
        assert_eq!(requests[0].ntlm_type(), 1);
        assert_eq!(server.connections(), 1, "{extra:?}");
    }
}

/// A server that offers no NTLM (Basic only, or Negotiate alone) is a
/// transport failure that names what it offers, after the type 1 alone:
/// nothing to log in with, and no refusal.
#[tokio::test]
async fn a_server_without_ntlm_is_named_and_gets_no_password() {
    for offered in [
        "WWW-Authenticate: Basic realm=\"mail.tum.de\"",
        "WWW-Authenticate: Negotiate",
    ] {
        let server = Server::start(vec![vec![(
            Some(response("401 Unauthorized", &[offered], "")),
            Then::Next,
        )]]);
        let http = transport(&server, Duration::from_secs(5));
        let err = http.post(ENVELOPE).await.unwrap_err();
        assert!(matches!(err, EwsFetchError::Transport(_)), "{err:?}");
        let offer = offered.trim_start_matches("WWW-Authenticate: ");
        assert_eq!(
            err.to_string(),
            format!(
                "could not reach the EWS endpoint: the Exchange server offers no NTLM login \
                 (WWW-Authenticate: {offer})"
            )
        );
        assert_eq!(server.requests().len(), 1);
    }
}

/// A logged-in connection that answers 401 lost its login (the server's
/// keep-alive ended it): the login runs again on a new connection, once, and
/// the call is answered there.
#[tokio::test]
async fn a_401_on_a_logged_in_connection_logs_in_again_on_a_new_one() {
    let server = Server::start(vec![
        vec![
            (Some(challenge(&[])), Then::Next),
            (Some(ok("<first/>")), Then::Next),
            (Some(refused()), Then::Next),
        ],
        vec![
            (Some(challenge(&[])), Then::Next),
            (Some(ok("<again/>")), Then::Next),
        ],
    ]);
    let http = transport(&server, Duration::from_secs(5));
    assert_eq!(http.post(ENVELOPE).await.unwrap(), "<first/>");
    assert_eq!(http.post(ENVELOPE).await.unwrap(), "<again/>");
    let legs: Vec<(usize, u32)> = server
        .requests()
        .iter()
        .map(|request| (request.connection, request.ntlm_type()))
        .collect();
    assert_eq!(legs, [(0, 1), (0, 3), (0, 0), (1, 1), (1, 3)]);
}

/// A SOAP fault comes back as HTTP 500 with its body, which the backend
/// reads; any other status is the seam's `Status`.
#[tokio::test]
async fn a_fault_is_an_answer_and_another_status_is_named() {
    let server = Server::start(vec![vec![
        (Some(challenge(&[])), Then::Next),
        (
            Some(response("500 Internal Server Error", &[], "<fault/>")),
            Then::Next,
        ),
        (
            Some(response("503 Service Unavailable", &[], "busy")),
            Then::Next,
        ),
    ]]);
    let http = transport(&server, Duration::from_secs(5));
    assert_eq!(http.post(ENVELOPE).await.unwrap(), "<fault/>");
    let err = http.post(ENVELOPE).await.unwrap_err();
    assert!(
        matches!(err, EwsFetchError::Status { status: 503, ref body } if body == "busy"),
        "{err:?}"
    );
}

/// A server that never answers fails the call at the request timeout as a
/// transport failure; one that is not there fails it at once.
#[tokio::test]
async fn a_silent_or_absent_server_is_a_transport_failure() {
    let server = Server::start(vec![vec![(None, Then::Next)]]);
    let http = transport(&server, Duration::from_millis(300));
    let started = std::time::Instant::now();
    let err = http.post(ENVELOPE).await.unwrap_err();
    assert!(matches!(err, EwsFetchError::Transport(_)), "{err:?}");
    assert!(err.to_string().contains("did not answer within"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(3));

    let unused = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/EWS/Exchange.asmx", unused.local_addr().unwrap());
    drop(unused);
    let http = EwsHttp::plain_for_test(
        &url,
        &fresh_user("absent"),
        "secret",
        timeouts(Duration::from_secs(5)),
    );
    let err = http.post(ENVELOPE).await.unwrap_err();
    assert!(matches!(err, EwsFetchError::Transport(_)), "{err:?}");
}

/// A user name written `DOMAIN\user` logs in as the user of that domain;
/// a bare one, TUM's, with an empty domain.
#[test]
fn a_user_name_splits_off_its_domain() {
    assert_eq!(split_user("ab12cde"), ("ab12cde", ""));
    assert_eq!(split_user("ADS\\ab12cde"), ("ab12cde", "ADS"));
    assert_eq!(split_user("ab12cde@tum.de"), ("ab12cde@tum.de", ""));
}

/// A refused EWS password holds the IMAP login of the same user and
/// password, and the reverse: TUM counts refusals per directory account
/// whatever the protocol, so the gate's one row per user and password holds
/// every login that presents them. A held EWS login dials nothing and says
/// where the password was refused and when it is asked again; another
/// password is asked at once.
#[tokio::test]
async fn a_refused_ews_password_holds_the_imap_login_of_its_user_and_the_reverse() {
    let user = fresh_user("refused");
    let server = Server::start(vec![
        vec![
            (Some(challenge(&[])), Then::Next),
            (Some(refused()), Then::Next),
        ],
        vec![
            (Some(challenge(&[])), Then::Next),
            (Some(ok("<taken/>")), Then::Next),
        ],
    ]);
    let pass =
        |password: &str| transport_as(&server, &user, password, timeouts(Duration::from_secs(5)));
    let err = pass("wrong").post(ENVELOPE).await.unwrap_err();
    assert!(matches!(err, EwsFetchError::LoginRefused(_)), "{err:?}");
    let imap = login_gate::dial(&imap_login(&user), "wrong")
        .err()
        .expect("the IMAP login of the same user and password is held");
    assert!(
        imap.to_string()
            .contains("(the server refused this password at the EWS login earlier"),
        "{imap}"
    );
    let held = pass("wrong").post(ENVELOPE).await.unwrap_err();
    assert!(
        matches!(held, EwsFetchError::LoginRefused(ref why)
            if why.starts_with("EWS login failed: HTTP 401") && why.contains("is asked again at")),
        "{held:?}"
    );
    assert_eq!(server.connections(), 1, "the held login dialled nothing");
    assert_eq!(pass("right").post(ENVELOPE).await.unwrap(), "<taken/>");
    assert_eq!(server.connections(), 2);

    let other = fresh_user("refused-at-imap");
    login_gate::record(
        &imap_login(&other),
        "wrong",
        &login_gate::LoginRejected::new(RejectedLogin::Imap, "NO LOGIN failed.".into()),
    );
    let quiet = Server::start(Vec::new());
    let held = transport_as(&quiet, &other, "wrong", timeouts(Duration::from_secs(5)))
        .post(ENVELOPE)
        .await
        .unwrap_err();
    assert!(
        matches!(held, EwsFetchError::LoginRefused(ref why)
            if why.contains("(the server refused this password at the IMAP login earlier")),
        "{held:?}"
    );
    assert_eq!(quiet.connections(), 0, "the EWS login dialled nothing");
}

/// The gate's deadline rule for a login that gets no answer: a type 3 of a
/// password the gate holds a refusal of, written and unanswered by the
/// deadline, is one more step of its hold; a type 3 of a password never
/// refused counts nothing; and a deadline that runs out before the type 3
/// was written (the server never answers the type 1) counts nothing, even
/// for a refused password, since no password reached the server.
#[tokio::test]
async fn an_unanswered_type_3_steps_the_hold_of_a_refused_password_only() {
    let short = Timeouts {
        login: Duration::from_millis(300),
        ..timeouts(Duration::from_secs(5))
    };
    let silent_after_challenge = || {
        Server::start(vec![vec![
            (Some(challenge(&[])), Then::Next),
            (None, Then::Next),
        ]])
    };

    let user = fresh_user("unanswered");
    let refused_once = |user: &str| {
        let login = imap_login(user);
        login_gate::record(
            &login,
            "wrong",
            &login_gate::LoginRejected::new(RejectedLogin::Imap, "NO LOGIN failed.".into()),
        );
        // The user acting ends the hold and keeps the row's count.
        login_gate::forget(&login);
        assert_eq!(login_gate::in_a_row_for_test(&login, "wrong"), 1);
        login
    };
    let login = refused_once(&user);
    let server = silent_after_challenge();
    let started = std::time::Instant::now();
    let err = transport_as(&server, &user, "wrong", short)
        .post(ENVELOPE)
        .await
        .unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert!(matches!(err, EwsFetchError::Transport(_)), "{err:?}");
    assert!(
        err.to_string()
            .contains("did not answer the NTLM login within"),
        "{err}"
    );
    assert_eq!(
        server
            .requests()
            .iter()
            .map(Request::ntlm_type)
            .collect::<Vec<_>>(),
        [1, 3]
    );
    assert_eq!(
        login_gate::in_a_row_for_test(&login, "wrong"),
        2,
        "a step of the hold"
    );
    assert!(
        login_gate::dial(&login, "wrong").is_err(),
        "held from then on"
    );

    let fresh = fresh_user("unanswered-fresh");
    let server = silent_after_challenge();
    let err = transport_as(&server, &fresh, "fresh", short)
        .post(ENVELOPE)
        .await
        .unwrap_err();
    assert!(matches!(err, EwsFetchError::Transport(_)), "{err:?}");
    assert_eq!(
        login_gate::in_a_row_for_test(&imap_login(&fresh), "fresh"),
        0
    );

    let early = fresh_user("unanswered-early");
    let login = refused_once(&early);
    let server = Server::start(vec![vec![(None, Then::Next)]]);
    let err = transport_as(&server, &early, "wrong", short)
        .post(ENVELOPE)
        .await
        .unwrap_err();
    assert!(matches!(err, EwsFetchError::Transport(_)), "{err:?}");
    assert_eq!(server.requests().len(), 1, "the type 1 alone");
    assert_eq!(
        login_gate::in_a_row_for_test(&login, "wrong"),
        1,
        "nothing counted"
    );
    assert!(login_gate::dial(&login, "wrong").is_ok(), "the row is free");
}

/// A type 3 whose head went out and whose body the server never reads is
/// counted: the head carries the password's proof, so a login cut off while
/// the body is still being written (the deadline running out on a server
/// that stopped reading) is the gate's unanswered login, a step of the hold
/// of a password refused before.
#[tokio::test]
async fn a_type_3_cut_off_after_its_head_steps_the_hold_of_a_refused_password() {
    // Larger than the loopback's socket buffers, so the client's write of
    // the type 3's body blocks once the server stops reading.
    let envelope = format!("<s:Envelope>{}</s:Envelope>", "x".repeat(32 * 1024 * 1024));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (heads, seen) = (
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(AtomicUsize::new(0)),
    );
    let (record, count) = (Arc::clone(&heads), Arc::clone(&seen));
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);
        let Some(negotiate) = read_request(0, &mut reader) else {
            return;
        };
        record.lock().unwrap().push(negotiate.ntlm_type());
        if writer.write_all(challenge(&[]).as_bytes()).is_err() {
            return;
        }
        let Some(authenticate) = read_head(0, &mut reader) else {
            return;
        };
        record.lock().unwrap().push(authenticate.ntlm_type());
        count.fetch_add(1, Ordering::SeqCst);
        // Hold the connection open and read nothing more.
        std::thread::sleep(Duration::from_secs(10));
    });
    let url = format!("http://127.0.0.1:{port}/EWS/Exchange.asmx");
    let (user, login) = refused_once("cut-off");
    let deadline = Timeouts {
        login: Duration::from_secs(2),
        ..timeouts(Duration::from_secs(10))
    };
    let err = EwsHttp::plain_for_test(&url, &user, "wrong", deadline)
        .post(&envelope)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("did not answer the NTLM login within"),
        "{err}"
    );
    assert_eq!(seen.load(Ordering::SeqCst), 1, "the type 3's head went out");
    assert_eq!(*heads.lock().unwrap(), [1, 3]);
    assert_eq!(
        login_gate::in_a_row_for_test(&login, "wrong"),
        2,
        "a step of the refused password's hold"
    );
    assert!(login_gate::dial(&login, "wrong").is_err(), "held");
}

/// A login the server takes ends the row of its password, count and all,
/// for every login of the row.
#[tokio::test]
async fn an_accepted_ews_login_ends_the_row_of_its_password() {
    let user = fresh_user("accepted");
    let login = imap_login(&user);
    login_gate::record(
        &login,
        "right",
        &login_gate::LoginRejected::new(RejectedLogin::Imap, "NO LOGIN failed.".into()),
    );
    login_gate::forget(&login);
    let server = Server::start(vec![vec![
        (Some(challenge(&[])), Then::Next),
        (Some(ok("<taken/>")), Then::Next),
    ]]);
    let http = transport_as(&server, &user, "right", timeouts(Duration::from_secs(5)));
    assert_eq!(http.post(ENVELOPE).await.unwrap(), "<taken/>");
    assert_eq!(login_gate::in_a_row_for_test(&login, "right"), 0);
}

/// The answer `raw` as a connection would read it.
fn answer_of(raw: &[u8]) -> Result<Answer> {
    let mut connection = Connection {
        stream: Box::new(futures::io::Cursor::new(raw.to_vec())),
        buffered: Vec::new(),
        last_used: std::time::Instant::now(),
    };
    futures::executor::block_on(read_answer(&mut connection))
}

/// A hostile chunk size after a first chunk is refused before anything is
/// read for it, where the sum of the body read and the size overflowed (a
/// panic in a debug build, a read until the server closes in a release
/// one); a Content-Length past the cap, or one no `usize` holds, is refused
/// the same way.
#[test]
fn a_hostile_body_size_is_refused_before_it_is_read() {
    let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
                    1\r\na\r\nffffffffffffffff\r\n";
    let err = answer_of(chunked).err().expect("refused");
    assert_eq!(
        err.to_string(),
        format!("an answer over the {MAX_BODY} bytes the transport reads")
    );
    let past_cap = format!(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\na\r\n{:x}\r\n",
        MAX_BODY
    );
    assert!(answer_of(past_cap.as_bytes()).is_err());
    let at_cap = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
                   1\r\na\r\n2\r\nbc\r\n0\r\n\r\n";
    assert_eq!(answer_of(at_cap).unwrap().body, b"abc");
    for length in [
        format!("{}", MAX_BODY + 1),
        "99999999999999999999999".to_string(),
    ] {
        let raw = format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\n\r\n");
        assert!(answer_of(raw.as_bytes()).is_err(), "{length}");
    }
}

/// The IMAP login of a fresh user whose password `wrong` the gate holds a
/// refusal of, its hold ended by the user acting, so the next dial asks.
fn refused_once(what: &str) -> (String, Login) {
    let user = fresh_user(what);
    let login = imap_login(&user);
    login_gate::record(
        &login,
        "wrong",
        &login_gate::LoginRejected::new(RejectedLogin::Imap, "NO LOGIN failed.".into()),
    );
    login_gate::forget(&login);
    assert_eq!(login_gate::in_a_row_for_test(&login, "wrong"), 1);
    (user, login)
}

/// After the type 3 went out, an answer that is neither a refusal nor one
/// past the login (a 503), a connection reset with no answer, and the call's
/// own timeout cancelling the login are each counted as the gate's
/// unanswered login: a step of the hold of a password refused before, and
/// nothing for a password never refused.
#[tokio::test]
async fn an_outcome_after_the_type_3_that_is_no_answer_steps_a_refused_password_only() {
    let unavailable = response("503 Service Unavailable", &[], "busy");
    let reset = String::new();
    for (what, answer, then) in [
        ("odd-status", Some(unavailable), Then::Next),
        ("reset", Some(reset), Then::Close),
    ] {
        let script = || {
            vec![vec![
                (Some(challenge(&[])), Then::Next),
                (answer.clone(), then.clone()),
            ]]
        };
        let (user, login) = refused_once(what);
        let server = Server::start(script());
        let err = transport_as(&server, &user, "wrong", timeouts(Duration::from_secs(5)))
            .post(ENVELOPE)
            .await
            .unwrap_err();
        assert!(
            !matches!(err, EwsFetchError::LoginRefused(_)),
            "{what}: {err:?}"
        );
        assert_eq!(
            login_gate::in_a_row_for_test(&login, "wrong"),
            2,
            "{what}: a step of the refused password's hold"
        );
        assert!(login_gate::dial(&login, "wrong").is_err(), "{what}: held");

        let fresh = fresh_user(&format!("{what}-fresh"));
        let server = Server::start(script());
        transport_as(&server, &fresh, "fresh", timeouts(Duration::from_secs(5)))
            .post(ENVELOPE)
            .await
            .unwrap_err();
        assert_eq!(
            login_gate::in_a_row_for_test(&imap_login(&fresh), "fresh"),
            0,
            "{what}: a password never refused counts nothing"
        );
        assert!(login_gate::dial(&imap_login(&fresh), "fresh").is_ok());
    }

    // The call's timeout, shorter here than the login's deadline, cancels a
    // login whose type 3 went out and got no answer.
    let (user, login) = refused_once("cancelled");
    let server = Server::start(vec![vec![
        (Some(challenge(&[])), Then::Next),
        (None, Then::Next),
    ]]);
    let cancelling = Timeouts {
        request: Duration::from_millis(300),
        ..timeouts(Duration::from_secs(5))
    };
    let err = transport_as(&server, &user, "wrong", cancelling)
        .post(ENVELOPE)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("did not answer within"), "{err}");
    assert_eq!(server.requests().len(), 2, "the type 3 went out");
    assert_eq!(login_gate::in_a_row_for_test(&login, "wrong"), 2);
    assert!(login_gate::dial(&login, "wrong").is_err(), "held");

    // Cancelled before its type 3 went out, it counts nothing.
    let (user, login) = refused_once("cancelled-early");
    let server = Server::start(vec![vec![(None, Then::Next)]]);
    transport_as(&server, &user, "wrong", cancelling)
        .post(ENVELOPE)
        .await
        .unwrap_err();
    assert_eq!(login_gate::in_a_row_for_test(&login, "wrong"), 1);
    assert!(login_gate::dial(&login, "wrong").is_ok(), "the row is free");
}
