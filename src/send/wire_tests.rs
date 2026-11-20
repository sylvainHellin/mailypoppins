//! The SMTP conversation over the wire the product really speaks (PERSO-210):
//! [`build_smtp_transport_with`]'s STARTTLS branch against the fake server of
//! `tests/support/fake_smtp.rs`, so each step's refusal reaches the verdict
//! and the words the outbox keeps, and the login deadline runs against a
//! server that does not answer.
//!
//! The plain-TCP fakes in `super::tests` pin the same submission loop
//! through `builder_dangerous`; these pin it through the TLS transport every
//! submission is built with.

use super::*;
use crate::send::fake_smtp::{RecordingSmtpServer, SmtpAnswer, SmtpStep};

/// How long a test lets an unanswered login run before the gate counts it:
/// short, so no test waits out the production minute.
const DEADLINE: std::time::Duration = std::time::Duration::from_millis(300);

/// An account of its own on the fake server, a user and an account name per
/// test so the gate's process-wide list keeps them apart.
fn config(server: &RecordingSmtpServer, user: &str) -> SmtpConfig {
    SmtpConfig {
        account: format!("wire-{user}"),
        host: "127.0.0.1".to_string(),
        port: server.port(),
        username: format!("{user}@example.com"),
        password: format!("{user}-password"),
        default_from: format!("{user}@example.com"),
        accept_invalid_certs: true,
    }
}

/// A message to `recipients`, its body carrying a line that starts with a
/// dot, which the wire doubles and the server undoes.
fn built(id: &str, recipients: &[&str]) -> BuiltMessage {
    BuiltMessage {
        raw: format!(
            "From: me@example.com\r\nTo: {}\r\nMessage-ID: <{id}@example.com>\r\n\
             Subject: wire\r\n\r\nfirst line\r\n.dotted line\r\n",
            recipients.join(", ")
        )
        .into_bytes(),
        message_id: format!("<{id}@example.com>"),
        recipients: recipients
            .iter()
            .map(|r| (r.to_string(), RecipientRole::To))
            .collect(),
        from: "me@example.com".to_string(),
        draft_key: None,
        server_copy: None,
    }
}

/// The real submission over the real transport, with the test's deadline.
async fn submit_to(built: &BuiltMessage, config: &SmtpConfig) -> Result<SendResult> {
    let mailer = build_smtp_transport_with(config, std::time::Duration::from_secs(5))?;
    submit_over_within(built, config, &mailer, DEADLINE).await
}

/// The one recipient's result of a one-recipient send the server answered.
async fn verdict_of(server: &RecordingSmtpServer, user: &str) -> RecipientResult {
    let sent = submit_to(&built(user, &["ada@example.org"]), &config(server, user))
        .await
        .expect("the server answered past the login");
    sent.results.into_iter().next().expect("one recipient")
}

/// A send to two recipients goes out as one transaction each, over STARTTLS
/// and a login each, and the server holds the bytes that were built: lettre
/// doubles the leading dot and the server undoes it (RFC 5321 4.5.2), and
/// the CRLF before the closing `.` is lettre's own, a blank line the
/// message gains on the wire.
#[tokio::test]
async fn a_message_goes_out_whole_over_starttls() {
    let server = RecordingSmtpServer::start();
    let config = config(&server, "whole");
    let message = built("whole", &["ada@example.org", "bob@example.org"]);

    let sent = submit_to(&message, &config)
        .await
        .expect("the send went out");

    assert!(sent.all_succeeded(), "{sent:?}");
    let deliveries = server.deliveries();
    let envelopes: Vec<(String, Vec<String>)> = deliveries
        .iter()
        .map(|d| (d.from.clone(), d.recipients.clone()))
        .collect();
    assert_eq!(
        envelopes,
        vec![
            (
                "me@example.com".to_string(),
                vec!["ada@example.org".to_string()]
            ),
            (
                "me@example.com".to_string(),
                vec!["bob@example.org".to_string()]
            ),
        ],
        "one envelope per recipient, so a Bcc never shows in another's"
    );
    let wire = [message.raw.as_slice(), b"\r\n"].concat();
    for delivery in &deliveries {
        assert!(delivery.acknowledged);
        assert_eq!(
            String::from_utf8_lossy(&delivery.data),
            String::from_utf8_lossy(&wire)
        );
    }
    let logins = server.logins();
    assert_eq!(logins.len(), 2, "a login per recipient: {logins:?}");
    assert!(logins
        .iter()
        .all(|l| l.username == config.username && l.password == config.password));
}

/// Each step's refusal reaches the recipient's verdict in the server's own
/// words, which is what the outbox row and the send report show: a `4xx` is
/// retryable, a `5xx` past the greeting is final.
#[tokio::test]
async fn each_refused_step_is_the_recipients_verdict_in_the_servers_words() {
    let cases = [
        (
            SmtpStep::Ehlo,
            "421 4.3.2 Service shutting down",
            RecipientVerdict::Retryable,
            "421 4.3.2 Service shutting down",
        ),
        (
            SmtpStep::Mail,
            "451 4.3.0 Error: queue file write error",
            RecipientVerdict::Retryable,
            "451 4.3.0 Error: queue file write error",
        ),
        (
            SmtpStep::Rcpt,
            "550 5.1.1 <ada@example.org>: Recipient address rejected: User unknown",
            RecipientVerdict::Rejected,
            "550 5.1.1 <ada@example.org>: Recipient address rejected: User unknown",
        ),
        (
            SmtpStep::Data,
            "554 5.5.1 Error: no valid recipients",
            RecipientVerdict::Rejected,
            "554 5.5.1 Error: no valid recipients",
        ),
        (
            SmtpStep::Message,
            "452 4.3.1 Insufficient system storage",
            RecipientVerdict::Retryable,
            "452 4.3.1 Insufficient system storage",
        ),
    ];
    for (step, reply, verdict, words) in cases {
        let server = RecordingSmtpServer::start();
        server.answer(step, Some(SmtpAnswer::reply(reply)));
        let result = verdict_of(&server, &format!("refused-{}", server.port())).await;
        assert_eq!(result.verdict, verdict, "{step:?}: {result:?}");
        assert_eq!(result.error.as_deref(), Some(words), "{step:?}");
        assert!(
            server.deliveries().is_empty() || step == SmtpStep::Message,
            "{step:?}: nothing reached the server's queue"
        );
    }
}

/// A server that will not serve at all says so in its greeting (RFC 5321
/// 3.1), and the send stops before any login.
#[tokio::test]
async fn a_refusing_greeting_is_answered_before_any_login() {
    let server = RecordingSmtpServer::start();
    server.answer(
        SmtpStep::Greeting,
        Some(SmtpAnswer::reply("554 5.3.2 No SMTP service here")),
    );
    let result = verdict_of(&server, "greeting").await;
    assert_eq!(
        result.error.as_deref(),
        Some("554 5.3.2 No SMTP service here")
    );
    assert!(!result.success);
    assert!(server.logins().is_empty(), "no password went out");
}

/// A reply over several lines (RFC 5321 4.2.1, `550-` then `550 `) reads as
/// one sentence, its code once and the enhanced code it repeats once, and a
/// reply with no text after its code still says what the code means; neither
/// is lettre's `permanent error (550): ...`. (A code with not even the space
/// after it is a reply lettre 0.11.19 cannot parse, read as no verdict;
/// lettre 0.11.24 accepts it.)
#[tokio::test]
async fn a_multi_line_reply_reads_as_one_sentence_and_a_bare_code_still_says_something() {
    let cases = [
        (
            "550-5.1.1 <ada@example.org>: Recipient address rejected:\r\n\
             550 5.1.1 User unknown in virtual mailbox table",
            RecipientVerdict::Rejected,
            "550 5.1.1 <ada@example.org>: Recipient address rejected: User unknown in virtual \
             mailbox table",
        ),
        (
            "451-4.7.1 Greylisted, see\r\n451-4.7.1 https://example.org/greylisting\r\n\
             451 4.7.1 try again in 5 minutes",
            RecipientVerdict::Retryable,
            "451 4.7.1 Greylisted, see https://example.org/greylisting try again in 5 minutes",
        ),
        (
            "550 ",
            RecipientVerdict::Rejected,
            "550 (the server gave no reason; a permanent refusal)",
        ),
        (
            "450 ",
            RecipientVerdict::Retryable,
            "450 (the server gave no reason; a temporary refusal)",
        ),
    ];
    for (reply, verdict, words) in cases {
        let server = RecordingSmtpServer::start();
        server.answer(SmtpStep::Rcpt, Some(SmtpAnswer::reply(reply)));
        let result = verdict_of(&server, &format!("lines-{}", server.port())).await;
        assert_eq!(result.verdict, verdict, "{reply:?}: {result:?}");
        assert_eq!(result.error.as_deref(), Some(words), "{reply:?}");
    }
}

/// A refused login over several lines is still a refused login: the gate
/// reads the code and the enhanced code `5.7.8` of the reply, never the
/// words it is shown in (PERSO-296).
#[tokio::test]
async fn a_multi_line_login_refusal_is_a_refused_login_in_the_servers_words() {
    let server = RecordingSmtpServer::start();
    server.answer(
        SmtpStep::Auth,
        Some(SmtpAnswer::reply(
            "535-5.7.8 Username and Password not accepted. For more information, go to\r\n\
             535 5.7.8  https://support.example.org/mail/?p=BadCredentials",
        )),
    );
    let refused = submit_to(
        &built("lines-login", &["ada@example.org"]),
        &config(&server, "lines-login"),
    )
    .await
    .expect_err("a send no recipient took fails on the login");
    let rejected = refused
        .downcast_ref::<crate::login_gate::LoginRejected>()
        .expect("the refusal is a refused login");
    assert_eq!(
        rejected.to_string(),
        "SMTP login failed: 535 5.7.8 Username and Password not accepted. For more \
         information, go to https://support.example.org/mail/?p=BadCredentials"
    );
}

/// One recipient refused leaves the others delivered: each is its own
/// transaction, so the refusal is that recipient's alone.
#[tokio::test]
async fn a_refused_recipient_leaves_the_others_delivered() {
    let server = RecordingSmtpServer::start();
    server.answer_for(
        "bob@example.org",
        SmtpStep::Rcpt,
        Some(SmtpAnswer::reply(
            "550 5.1.1 <bob@example.org>: Recipient address rejected: User unknown",
        )),
    );
    let message = built("partly", &["ada@example.org", "bob@example.org"]);
    let sent = submit_to(&message, &config(&server, "partly"))
        .await
        .expect("the send went out");
    let verdicts: Vec<_> = sent.results.iter().map(|r| r.verdict).collect();
    assert_eq!(
        verdicts,
        vec![RecipientVerdict::Delivered, RecipientVerdict::Rejected]
    );
    let delivered: Vec<_> = server
        .deliveries()
        .into_iter()
        .flat_map(|d| d.recipients)
        .collect();
    assert_eq!(delivered, vec!["ada@example.org".to_string()]);
}

/// A login the server refuses is the send's error, in the words the account
/// shows, and the recipients after it are not asked: the login is the same
/// for each, so asking again would only spend the password.
#[tokio::test]
async fn a_refused_login_is_the_servers_answer_and_stops_the_send() {
    let server = RecordingSmtpServer::start();
    server.answer(
        SmtpStep::Auth,
        Some(SmtpAnswer::reply("535 5.7.8 Error: authentication failed")),
    );
    let message = built("refused-login", &["ada@example.org", "bob@example.org"]);
    let refused = submit_to(&message, &config(&server, "refused-login"))
        .await
        .expect_err("a send no recipient took fails on the login");
    let rejected = refused
        .downcast_ref::<crate::login_gate::LoginRejected>()
        .expect("the refusal is a refused login");
    assert_eq!(
        rejected.to_string(),
        "SMTP login failed: 535 5.7.8 Error: authentication failed"
    );
    assert_eq!(server.logins().len(), 1, "one AUTH, not one per recipient");
    assert!(server.deliveries().is_empty());
}

/// A connection lost after the message went in and before the server's
/// answer leaves the recipient with no verdict: the server holds the
/// message, which the client cannot know, so the send is neither delivered
/// nor safe to repeat.
#[tokio::test]
async fn a_message_the_server_hung_up_on_has_no_verdict_and_was_received() {
    let server = RecordingSmtpServer::start();
    server.answer(SmtpStep::Message, Some(SmtpAnswer::HangUp));
    let result = verdict_of(&server, "hung-up").await;
    assert_eq!(result.verdict, RecipientVerdict::Unknown, "{result:?}");
    assert_eq!(
        result.error.as_deref(),
        Some("the connection closed before the server answered")
    );
    let deliveries = server.deliveries();
    assert_eq!(deliveries.len(), 1, "the server has the message");
    assert!(!deliveries[0].acknowledged);
}

/// Without the loopback opt-in, the fake's self-signed certificate is
/// refused in the TLS handshake, before any password goes out.
#[tokio::test]
async fn an_untrusted_certificate_is_refused_before_the_login() {
    let server = RecordingSmtpServer::start();
    let mut config = config(&server, "untrusted");
    config.accept_invalid_certs = false;
    let sent = submit_to(&built("untrusted", &["ada@example.org"]), &config)
        .await
        .expect("the send reports the recipient");
    let error = sent.results[0].error.clone().unwrap_or_default();
    assert!(
        error.starts_with("the SMTP server's certificate was not trusted")
            && error.ends_with("; nothing was sent"),
        "{error}"
    );
    assert_eq!(sent.results[0].verdict, RecipientVerdict::Unreached);
    assert!(server.logins().is_empty(), "no password went out");
}

/// PERSO-296 over TLS: the login of a password refused before that the
/// server leaves unanswered counts as one more step of the password's hold
/// once the deadline is out, and the send goes on waiting.
#[tokio::test]
async fn a_stalled_login_of_a_refused_password_is_a_step_of_its_hold() {
    use crate::login_gate::{self, Login, LoginRejected};
    let server = RecordingSmtpServer::start();
    server.answer(SmtpStep::Auth, Some(SmtpAnswer::Stall));
    let config = config(&server, "stalled");
    let login = Login::Smtp(config.login());
    let refusal = LoginRejected::new(
        mp_protocol::account::RejectedLogin::Smtp,
        "535 5.7.8".into(),
    );
    login_gate::record(&login, &config.password, &refusal);
    login_gate::forget(&login);

    let sending = {
        let config = config.clone();
        tokio::spawn(
            async move { submit_to(&built("stalled", &["ada@example.org"]), &config).await },
        )
    };
    let waited = std::time::Instant::now();
    while login_gate::in_a_row_for_test(&login, &config.password) < 2 {
        assert!(
            waited.elapsed() < std::time::Duration::from_secs(10),
            "the deadline never counted the unanswered login"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(waited.elapsed() >= DEADLINE, "not before the deadline");
    assert_eq!(server.reached(SmtpStep::Auth), 1, "the server has the AUTH");
    assert!(!sending.is_finished(), "the send goes on");
    sending.abort();
}

/// A recipient deferred for one transaction is retryable in the server's
/// words, and the next send to it is delivered: the deferral wears off as a
/// server's does once it is back.
#[tokio::test]
async fn a_deferral_that_wears_off_is_retryable_then_delivered() {
    let server = RecordingSmtpServer::start();
    server.answer_for_next(
        "ada@example.org",
        SmtpStep::Rcpt,
        SmtpAnswer::reply(
            "451 4.7.1 <ada@example.org>: Recipient address rejected: try again later",
        ),
        1,
    );
    let first = verdict_of(&server, "deferred").await;
    assert_eq!(first.verdict, RecipientVerdict::Retryable);
    assert_eq!(
        first.error.as_deref(),
        Some("451 4.7.1 <ada@example.org>: Recipient address rejected: try again later")
    );
    let second = verdict_of(&server, "deferred").await;
    assert_eq!(second.verdict, RecipientVerdict::Delivered, "{second:?}");
    assert_eq!(server.deliveries().len(), 1);
}
