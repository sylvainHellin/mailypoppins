//! PERSO-330: an SMTP server that accepts the connection and then says
//! nothing ends the submission at its deadline, with no verdict, instead of
//! holding it, and the outbox row in "sending", until the daemon restarts.
//!
//! Against the STARTTLS fake the daemon suites use, through
//! [`submit_over_bounded`] with a budget of its own ([`BUDGET`]), so no test
//! waits minutes: once the fake has fallen silent, the test stops tokio's
//! clock ([`at_its_silence`]) and the deadline fires at once, however loaded
//! the machine, rather than after a stretch of wall-clock time a slow
//! conversation could outrun.
//!
//! Beside them, a connection never set up, which sent nothing and says so.

use super::*;
use crate::outbox::{self, OutboxState, SubmitOutcome};
use crate::store::{BlobStore, Store};
use std::time::Duration;

use super::fake_smtp::{RecordingSmtpServer, SmtpAnswer, SmtpStep};

/// Where the fake falls silent: it stops answering after that step, holding
/// the connection open, as a hung server does.
#[derive(Clone, Copy)]
enum SilentAfter {
    /// Accepts the connection and sends no greeting.
    Accept,
    /// Takes the login and answers nothing to it.
    Auth,
    /// Takes the whole message and answers nothing to its final dot.
    Data,
}

/// The fake as these tests drive it: silent from a step on, and how many
/// messages it took whole.
trait Silent {
    fn start_silent(silence: SilentAfter) -> Self;
    fn messages(&self) -> usize;
    /// The fake came to `silence` and has said nothing since, with what
    /// the test checks there recorded.
    fn fell_silent(&self, silence: SilentAfter) -> bool;
}

impl Silent for RecordingSmtpServer {
    fn start_silent(silence: SilentAfter) -> Self {
        let server = RecordingSmtpServer::start();
        let step = match silence {
            SilentAfter::Accept => SmtpStep::Greeting,
            SilentAfter::Auth => SmtpStep::Auth,
            SilentAfter::Data => SmtpStep::Message,
        };
        server.answer(step, Some(SmtpAnswer::Stall));
        server
    }

    fn messages(&self) -> usize {
        self.deliveries().len()
    }

    fn fell_silent(&self, silence: SilentAfter) -> bool {
        match silence {
            SilentAfter::Accept => self.reached(SmtpStep::Greeting) > 0,
            // The login is recorded before the step is counted.
            SilentAfter::Auth => self.reached(SmtpStep::Auth) > 0,
            // The message is recorded after the step is counted.
            SilentAfter::Data => self.messages() > 0,
        }
    }
}

/// The whole conversation, nothing for the upload: 20 s, a hundred times what
/// the fake takes to come to any of its silences, so on a loaded machine too
/// the deadline cuts a conversation only where the fake fell silent. A test
/// does not wait it out: [`at_its_silence`] stops the clock there. Under the
/// transport's 30 s connect timeout, so the deadline is the first timer due.
const BUDGET: SubmissionBudget = SubmissionBudget {
    conversation: Duration::from_secs(20),
    per_mib: Duration::ZERO,
};

/// Long past [`BUDGET`]: a submission still running then never ends.
const NEVER: Duration = Duration::from_secs(60);

/// Drive `submission` until `server` falls silent at `silence`, then stop
/// tokio's clock: with nothing more coming from the fake, the runtime idles
/// and the clock jumps to its next timer, the submission's deadline, which
/// fires at once rather than after [`BUDGET`] of wall-clock time; a
/// submission with no deadline runs on to [`NEVER`] and fails its test all
/// the same. A submission that ended before the fake fell silent is handed
/// back as it is, for the test's assertions to catch.
///
/// Tokio's clock stops only on a current-thread runtime, `#[tokio::test]`'s
/// default, and stays stopped to the end of the test.
async fn at_its_silence<T>(
    server: &RecordingSmtpServer,
    silence: SilentAfter,
    submission: impl std::future::Future<Output = T>,
) -> T {
    tokio::pin!(submission);
    let silent = async {
        while !server.fell_silent(silence) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    tokio::select! {
        ended = &mut submission => return ended,
        () = silent => {}
    }
    tokio::time::pause();
    submission.await
}

/// The fake's account, with a user of its own per port, since the login
/// gate keys a login by its user and tests run side by side.
fn config(port: u16) -> SmtpConfig {
    SmtpConfig {
        account: format!("deadline-{port}"),
        host: "127.0.0.1".to_string(),
        port,
        username: format!("deadline-{port}@example.com"),
        password: "the-password".to_string(),
        default_from: "me@example.com".to_string(),
        accept_invalid_certs: true,
    }
}

fn built(message_id: &str, recipients: &[&str]) -> BuiltMessage {
    BuiltMessage {
        raw: format!("Message-ID: {message_id}\r\nSubject: x\r\n\r\nbody\r\n").into_bytes(),
        message_id: message_id.to_string(),
        recipients: recipients
            .iter()
            .map(|r| (r.to_string(), RecipientRole::To))
            .collect(),
        from: "me@example.com".to_string(),
        draft_key: None,
        server_copy: None,
    }
}

/// The real submission over the production transport, with [`BUDGET`], to
/// a fake that falls silent at a step of the conversation, whose deadline
/// then fires at once ([`at_its_silence`]), or that never does.
struct BoundedSmtp<'a> {
    silent: Option<(&'a RecordingSmtpServer, SilentAfter)>,
}

/// [`BoundedSmtp`] to a server that answers, or that is never reached.
const ANSWERING: BoundedSmtp<'static> = BoundedSmtp { silent: None };

impl SmtpSubmit for BoundedSmtp<'_> {
    async fn submit(&self, built: &BuiltMessage, smtp_config: &SmtpConfig) -> Result<SendResult> {
        let mailer = build_smtp_transport(smtp_config)?;
        let submission = submit_over_bounded(
            built,
            smtp_config,
            &mailer,
            crate::login_gate::LOGIN_DEADLINE,
            BUDGET,
        );
        match self.silent {
            None => submission.await,
            Some((server, silence)) => at_its_silence(server, silence, submission).await,
        }
    }
}

/// Submit `built` to `server`, which answers every step, failing the test
/// if the submission outlives [`NEVER`].
async fn submit_to(server: &RecordingSmtpServer, built: &BuiltMessage) -> SendResult {
    submit_with(&config(server.port()), built, &ANSWERING).await
}

/// [`submit_to`] a server that falls silent at `silence`.
async fn submit_to_silent(
    server: &RecordingSmtpServer,
    silence: SilentAfter,
    built: &BuiltMessage,
) -> SendResult {
    let smtp = BoundedSmtp {
        silent: Some((server, silence)),
    };
    submit_with(&config(server.port()), built, &smtp).await
}

/// [`submit_to`] with the configuration and the submission handed in.
async fn submit_with(
    config: &SmtpConfig,
    built: &BuiltMessage,
    smtp: &BoundedSmtp<'_>,
) -> SendResult {
    tokio::time::timeout(NEVER, smtp.submit(built, config))
        .await
        .expect("the submission ends at its deadline")
        .expect("a submission that ran out of time is a result, not an error")
}

/// The one recipient of `result` ended with no verdict: not delivered, not
/// refused, so the outbox parks the row and never re-sends it on its own.
fn assert_no_verdict(result: &SendResult) {
    let recipient = &result.results[0];
    assert_eq!(
        recipient.verdict,
        RecipientVerdict::Unknown,
        "{:?}",
        recipient.error
    );
    assert!(!recipient.success);
    let error = recipient.error.as_deref().unwrap_or_default();
    assert!(
        error.contains("no answer") && error.contains("may have"),
        "the reason says the server went quiet and the message may be there: {error}"
    );
    let SubmitOutcome::PerRecipient(verdicts) = result.submit_outcome() else {
        panic!("an SMTP send has a verdict per recipient");
    };
    assert_eq!(verdicts.ambiguous.len(), 1);
    assert!(verdicts.rejected.is_empty());
    assert!(verdicts.delivered.is_empty());
}

/// The control: the same fake answering every step delivers within the
/// budget, so a test below that runs out of time does so at its silence.
#[tokio::test]
async fn a_server_that_answers_is_delivered_within_the_budget() {
    let server = RecordingSmtpServer::start();
    let result = submit_to(
        &server,
        &built("<answered@example.com>", &["bob@example.com"]),
    )
    .await;
    assert!(result.all_succeeded(), "{:?}", result.results);
    assert_eq!(server.messages(), 1);
}

#[tokio::test]
async fn a_server_silent_after_accept_ends_the_send_with_no_verdict() {
    let server = RecordingSmtpServer::start_silent(SilentAfter::Accept);
    let result = submit_to_silent(
        &server,
        SilentAfter::Accept,
        &built("<silent-accept@example.com>", &["bob@example.com"]),
    )
    .await;
    assert_no_verdict(&result);
    assert!(server.logins().is_empty());
}

#[tokio::test]
async fn a_server_silent_after_auth_ends_the_send_with_no_verdict() {
    let server = RecordingSmtpServer::start_silent(SilentAfter::Auth);
    let result = submit_to_silent(
        &server,
        SilentAfter::Auth,
        &built("<silent-auth@example.com>", &["bob@example.com"]),
    )
    .await;
    assert_no_verdict(&result);
    assert_eq!(server.logins().len(), 1, "the login reached the server");
    assert_eq!(server.messages(), 0);
}

/// The server holds the whole message and never says whether it took it:
/// the recipient has no verdict, never "rejected".
#[tokio::test]
async fn a_server_silent_after_the_final_dot_ends_the_send_with_no_verdict() {
    let server = RecordingSmtpServer::start_silent(SilentAfter::Data);
    let result = submit_to_silent(
        &server,
        SilentAfter::Data,
        &built("<silent-data@example.com>", &["bob@example.com"]),
    )
    .await;
    assert_eq!(server.messages(), 1, "the message reached the server whole");
    assert_no_verdict(&result);
}

/// The recipients after one whose submission ran out of time are not tried,
/// each of which would wait out a deadline of its own against a server that
/// has stopped answering: they are left for a later send. Tokio's clock
/// stops once the fake falls silent ([`at_its_silence`]), so the time the
/// send took is the first deadline and nothing more, whatever the load.
#[tokio::test]
async fn the_recipients_after_a_deadline_are_left_for_a_later_send() {
    let server = RecordingSmtpServer::start_silent(SilentAfter::Data);
    let started = tokio::time::Instant::now();
    let result = submit_to_silent(
        &server,
        SilentAfter::Data,
        &built(
            "<silent-data-two@example.com>",
            &["bob@example.com", "carol@example.com"],
        ),
    )
    .await;
    assert!(
        started.elapsed() < BUDGET.conversation * 2,
        "the second recipient waited out no deadline of its own"
    );
    assert_eq!(result.results[0].verdict, RecipientVerdict::Unknown);
    let carol = &result.results[1];
    assert_eq!(carol.address, "carol@example.com");
    assert_eq!(carol.verdict, RecipientVerdict::Retryable);
    assert!(
        carol
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with("not sent")),
        "{:?}",
        carol.error
    );
    assert_eq!(server.messages(), 1, "carol's copy was never uploaded");
}

/// A drain resubmitting a queued row against a server gone silent parks the
/// row as failed with no verdict, which only `mp outbox retry` sends again,
/// instead of leaving it in "sending".
#[tokio::test]
async fn a_drain_against_a_silent_server_parks_the_row() {
    let server = RecordingSmtpServer::start_silent(SilentAfter::Data);
    let row = drained(
        &config(server.port()),
        &built("<silent-drain@example.com>", &["bob@example.com"]),
        &BoundedSmtp {
            silent: Some((&server, SilentAfter::Data)),
        },
    )
    .await;
    assert_eq!(row.state, OutboxState::Failed, "{:?}", row.last_error);
    assert!(
        row.last_error
            .as_deref()
            .is_some_and(|e| e.contains("no verdict for") && e.contains("no answer")),
        "{:?}",
        row.last_error
    );
}

/// `built` queued in a fresh outbox for `config`'s account and drained once
/// through `smtp`: its row as the drain left it.
async fn drained(
    config: &SmtpConfig,
    built: &BuiltMessage,
    smtp: &BoundedSmtp<'_>,
) -> outbox::OutboxRow {
    let account = config.account.clone();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
    let blobs = BlobStore::new(dir.path().join("blobs"));
    let row_id = outbox::enqueue(
        &store,
        &blobs,
        &account,
        None,
        &built.message_id,
        &built.raw,
        &outbox::Envelope {
            from: built.from.clone(),
            recipients: built.recipients.clone(),
            ..Default::default()
        },
    )
    .unwrap();
    let rows = outbox::pending_send_rows(&store, &account).unwrap();
    let drafts = dir.path().join("drafts");
    let drain = resubmit_rows(
        &store,
        &blobs,
        &account,
        &drafts,
        rows,
        outbox::unix_now(),
        config,
        smtp,
    );
    tokio::time::timeout(NEVER, drain)
        .await
        .expect("the drain ends at the submission's deadline");
    outbox::load(&store, row_id).unwrap().unwrap()
}

// ---------------------------------------------------------------------------
// A connection never set up: nothing was sent, and the words say so
// ---------------------------------------------------------------------------

/// A connection the transport never set up (the lookup, the connect or the
/// TLS handshake failed) sent nothing, and its reason says so in plain
/// words, both on the recipient's line and on the parked row: never "no
/// verdict" or "may have taken the message", which belong to a server that
/// went quiet mid-conversation. The row is parked as failed all the same:
/// whether a later drain sends it again on its own is a decision still
/// open, so its state and its retry path stay as they are.
async fn assert_nothing_sent(config: &SmtpConfig, reason_starts: &str) {
    let message_id = format!("<unreached-{}@example.com>", config.port);
    let result = submit_with(
        config,
        &built(&message_id, &["bob@example.com"]),
        &ANSWERING,
    )
    .await;
    let bob = &result.results[0];
    assert!(!bob.success);
    let error = bob.error.as_deref().unwrap_or_default();
    assert!(
        error.starts_with(reason_starts) && error.ends_with("; nothing was sent"),
        "the recipient's reason says why in plain words and that nothing was sent: {error}"
    );

    let row = drained(
        config,
        &built(&message_id, &["bob@example.com"]),
        &ANSWERING,
    )
    .await;
    assert_eq!(row.state, OutboxState::Failed, "{:?}", row.last_error);
    let last_error = row.last_error.unwrap_or_default();
    assert!(
        last_error.contains(reason_starts) && last_error.contains("nothing was sent"),
        "{last_error}"
    );
    assert!(
        !last_error.contains("no verdict") && !last_error.contains("may have"),
        "the parked row does not read as one the server may hold: {last_error}"
    );
}

#[tokio::test]
async fn a_refused_connect_says_nothing_was_sent() {
    // A port nothing listens on, held bound for the whole test without a
    // listen, which the kernel refuses. A port bound and let go is free for
    // the next bind of port 0, the fake of a test running beside this one
    // among them, which then took this test's message (PERSO-342).
    let unlistened = tokio::net::TcpSocket::new_v4().unwrap();
    unlistened.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let port = unlistened.local_addr().unwrap().port();
    assert_nothing_sent(
        &config(port),
        &format!("could not connect to 127.0.0.1:{port}: connection refused"),
    )
    .await;
}

#[tokio::test]
async fn a_host_that_does_not_resolve_says_nothing_was_sent() {
    // `.invalid` never resolves (RFC 6761), and the resolver says so at once.
    let config = SmtpConfig {
        host: "mp-smtp-test.invalid".to_string(),
        accept_invalid_certs: false,
        ..config(9)
    };
    assert_nothing_sent(
        &config,
        "could not look up the SMTP server mp-smtp-test.invalid",
    )
    .await;
}

#[tokio::test]
async fn an_untrusted_certificate_says_nothing_was_sent() {
    let server = RecordingSmtpServer::start();
    let config = SmtpConfig {
        accept_invalid_certs: false,
        ..config(server.port())
    };
    assert_nothing_sent(&config, "the SMTP server's certificate was not trusted").await;
    assert!(server.logins().is_empty(), "no password went out");
    assert_eq!(server.messages(), 0);
}

/// The budget of a submission is the conversation's allowance plus the
/// upload of the message at a slow but real rate, so a large message on a
/// slow link is never cut while it uploads.
#[test]
fn the_submission_deadline_grows_with_the_message() {
    let standard = SubmissionBudget::STANDARD;
    assert_eq!(standard.for_message(0), Duration::from_secs(150));
    assert_eq!(
        standard.for_message(1024 * 1024),
        Duration::from_secs(150 + 32)
    );
    assert_eq!(
        standard.for_message(25 * 1024 * 1024),
        Duration::from_secs(150 + 25 * 32)
    );
}
