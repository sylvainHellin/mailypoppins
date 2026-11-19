//! PERSO-330: an SMTP server that accepts the connection and then says
//! nothing ends the submission at its deadline, with no verdict, instead of
//! holding it, and the outbox row in "sending", until the daemon restarts.
//!
//! Against the STARTTLS fake the daemon suites use, with a budget of half a
//! second through [`submit_over_bounded`], so no test waits minutes.

use super::*;
use crate::outbox::{self, OutboxState, SubmitOutcome};
use crate::store::{BlobStore, Store};
use std::time::{Duration, Instant};

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
}

/// Half a second for the whole conversation, nothing for the upload.
const BUDGET: SubmissionBudget = SubmissionBudget {
    conversation: Duration::from_millis(500),
    per_mib: Duration::ZERO,
};

/// Long past [`BUDGET`]: a submission still running then never ends.
const NEVER: Duration = Duration::from_secs(10);

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

/// The real submission over the production transport, with [`BUDGET`].
struct BoundedSmtp;

impl SmtpSubmit for BoundedSmtp {
    async fn submit(&self, built: &BuiltMessage, smtp_config: &SmtpConfig) -> Result<SendResult> {
        let mailer = build_smtp_transport(smtp_config)?;
        submit_over_bounded(
            built,
            smtp_config,
            &mailer,
            crate::login_gate::LOGIN_DEADLINE,
            BUDGET,
        )
        .await
    }
}

/// Submit `built` to `server`, failing the test if the submission outlives
/// [`NEVER`].
async fn submit_to(server: &RecordingSmtpServer, built: &BuiltMessage) -> SendResult {
    let config = config(server.port());
    tokio::time::timeout(NEVER, BoundedSmtp.submit(built, &config))
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
    let result = submit_to(
        &server,
        &built("<silent-accept@example.com>", &["bob@example.com"]),
    )
    .await;
    assert_no_verdict(&result);
    assert!(server.logins().is_empty());
}

#[tokio::test]
async fn a_server_silent_after_auth_ends_the_send_with_no_verdict() {
    let server = RecordingSmtpServer::start_silent(SilentAfter::Auth);
    let result = submit_to(
        &server,
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
    let result = submit_to(
        &server,
        &built("<silent-data@example.com>", &["bob@example.com"]),
    )
    .await;
    assert_eq!(server.messages(), 1, "the message reached the server whole");
    assert_no_verdict(&result);
}

/// The recipients after one whose submission ran out of time are not tried,
/// each of which would wait out a deadline of its own against a server that
/// has stopped answering: they are left for a later send.
#[tokio::test]
async fn the_recipients_after_a_deadline_are_left_for_a_later_send() {
    let server = RecordingSmtpServer::start_silent(SilentAfter::Data);
    let started = Instant::now();
    let result = submit_to(
        &server,
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
/// row as failed with no verdict, which only `mp outbox retry` sends again
/// through the Sent search, instead of leaving it in "sending".
#[tokio::test]
async fn a_drain_against_a_silent_server_parks_the_row() {
    let server = RecordingSmtpServer::start_silent(SilentAfter::Data);
    let config = config(server.port());
    let account = config.account.clone();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
    let blobs = BlobStore::new(dir.path().join("blobs"));
    let built = built("<silent-drain@example.com>", &["bob@example.com"]);
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
        &config,
        &BoundedSmtp,
    );
    tokio::time::timeout(NEVER, drain)
        .await
        .expect("the drain ends at the submission's deadline");
    let row = outbox::load(&store, row_id).unwrap().unwrap();
    assert_eq!(row.state, OutboxState::Failed, "{:?}", row.last_error);
    assert!(
        row.last_error
            .as_deref()
            .is_some_and(|e| e.contains("no verdict for") && e.contains("no answer")),
        "{:?}",
        row.last_error
    );
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
