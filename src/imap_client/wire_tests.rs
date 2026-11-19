//! The client's server writes over the wire (PERSO-210): the moves, deletes,
//! flag changes and Sent APPENDs of `super::ops` and `super::sent` against
//! the fake server of `tests/support/fake_imap.rs`, which holds the folders
//! they change and answers as RFC 3501 and RFC 4315 say a server does.

use super::fake_imap::{LoginPolicy, RejectingImapServer};
use crate::config::ImapConfig;
use crate::outbox::SentMailbox;

/// A server that takes the login and holds `INBOX` with the messages
/// `ids`, under UIDs 1, 2, 3..., and an empty `Archive`.
fn server(ids: &[&str]) -> RejectingImapServer {
    let server = RejectingImapServer::start_with(LoginPolicy::Accept);
    server.set_folders(Vec::new());
    server.set_mail(
        "INBOX",
        7,
        ids.iter()
            .zip(1u32..)
            .map(|(id, uid)| (uid, mail(id)))
            .collect(),
    );
    server.set_mail("Archive", 9, Vec::new());
    server
}

/// A message whose Message-ID is `<id@example.com>`.
fn mail(id: &str) -> Vec<u8> {
    format!(
        "Message-ID: <{id}@example.com>\r\nFrom: ada@example.org\r\nSubject: about {id}\r\n\
         \r\nthe body of {id}\r\n"
    )
    .into_bytes()
}

fn id(id: &str) -> String {
    format!("<{id}@example.com>")
}

/// An account of its own on `server`, so the session pool keeps it apart.
fn config(server: &RejectingImapServer) -> ImapConfig {
    ImapConfig {
        account: format!("wire-{}", server.port()),
        host: "127.0.0.1".into(),
        port: server.port(),
        username: format!("wire-{}@example.com", server.port()),
        password: "wire".into(),
        accept_invalid_certs: true,
        fetch_concurrency: 4,
        body_fetch_deadline_secs: 30,
    }
}

/// `(uid, Message-ID, flags)` of each message of `folder`.
fn held(server: &RejectingImapServer, folder: &str) -> Vec<(u32, String, Vec<String>)> {
    server
        .contents(folder)
        .into_iter()
        .map(|m| (m.uid, m.message_id, m.flags))
        .collect()
}

/// A move is a COPY that keeps the flags, filed under the destination's
/// next UID, and the source copy flagged and expunged.
#[tokio::test]
async fn a_move_files_the_copy_with_its_flags_and_removes_the_source() {
    let server = server(&["one", "two", "three"]);
    server.set_flags("INBOX", 2, &["\\Seen"]);
    super::ops::move_email_on_server(&config(&server), &id("two"), "INBOX", None, "Archive")
        .await
        .expect("the move went through");
    let inbox: Vec<u32> = held(&server, "INBOX").into_iter().map(|m| m.0).collect();
    assert_eq!(inbox, vec![1, 3]);
    assert_eq!(
        held(&server, "Archive"),
        vec![(1, id("two"), vec!["\\Seen".to_string()])]
    );
}

/// With UIDPLUS a delete expunges its own copy and no other message
/// flagged `\Deleted`; without it, the plain EXPUNGE takes every one, the
/// risk `mark_deleted_and_expunge` documents.
#[tokio::test]
async fn a_delete_expunges_only_its_copy_where_the_server_offers_uidplus() {
    for (uidplus, left) in [(true, vec![1, 3]), (false, vec![3])] {
        let server = server(&["one", "two", "three"]);
        server.set_uidplus(uidplus);
        server.set_flags("INBOX", 1, &["\\Deleted"]);
        super::ops::delete_email_on_server(&config(&server), &id("two"), "INBOX", None)
            .await
            .expect("the delete went through");
        let inbox: Vec<u32> = held(&server, "INBOX").into_iter().map(|m| m.0).collect();
        assert_eq!(inbox, left, "UIDPLUS offered: {uidplus}");
    }
}

/// The read toggle and the flags change exactly the flag named.
#[tokio::test]
async fn the_flag_changes_store_exactly_the_flag_named() {
    let server = server(&["one"]);
    let config = config(&server);
    super::ops::mark_read_on_server(&config, &id("one"), "INBOX", None)
        .await
        .unwrap();
    super::ops::add_flag_on_server(&config, &id("one"), "INBOX", None, "\\Flagged")
        .await
        .unwrap();
    assert_eq!(held(&server, "INBOX")[0].2, vec!["\\Seen", "\\Flagged"]);
    super::ops::mark_unread_on_server(&config, &id("one"), "INBOX", None)
        .await
        .unwrap();
    super::ops::remove_flag_on_server(&config, &id("one"), "INBOX", None, "\\Flagged")
        .await
        .unwrap();
    assert!(held(&server, "INBOX")[0].2.is_empty());
}

/// A refused COPY fails the move in the server's words and leaves the
/// message where it was, neither flagged nor expunged.
#[tokio::test]
async fn a_refused_copy_fails_the_move_and_changes_nothing() {
    let server = server(&["one"]);
    server.refuse("COPY", Some("NO [OVERQUOTA] Quota exceeded"));
    let refused =
        super::ops::move_email_on_server(&config(&server), &id("one"), "INBOX", None, "Archive")
            .await
            .expect_err("the server refused the copy");
    let words = format!("{refused:#}");
    assert!(
        words.starts_with("Failed to copy email to Archive: ") && words.contains("Quota exceeded"),
        "{words}"
    );
    assert_eq!(held(&server, "INBOX"), vec![(1, id("one"), Vec::new())]);
    assert!(held(&server, "Archive").is_empty());
}

/// A move into a folder the server has not got is its `NO [TRYCREATE]`
/// (RFC 3501 6.4.7).
#[tokio::test]
async fn a_move_into_a_missing_folder_is_refused_with_trycreate() {
    let server = server(&["one"]);
    let refused =
        super::ops::move_email_on_server(&config(&server), &id("one"), "INBOX", None, "Nowhere")
            .await
            .expect_err("no such folder");
    assert_eq!(
        format!("{refused:#}"),
        "Failed to copy email to Nowhere: the server answered NO: [TRYCREATE] \
         Mailbox doesn't exist: Nowhere",
        "the server's own answer, its code first, which is what the op's error shows"
    );
    assert_eq!(held(&server, "INBOX").len(), 1);
}

/// The Sent APPEND files the message `\Seen` and reads its UID off
/// `APPENDUID`; without UIDPLUS the dedup search finds it instead, by its
/// Message-ID.
#[tokio::test]
async fn the_sent_append_files_the_copy_and_knows_its_uid() {
    for uidplus in [true, false] {
        let server = server(&[]);
        server.set_mail("Sent", 3, vec![(41, mail("older"))]);
        server.set_uidplus(uidplus);
        let mut sent = super::sent::ImapSentMailbox::new(config(&server));
        let uid = sent.append("Sent", &mail("new")).await.expect("filed");
        assert_eq!(uid, Some(42), "UIDPLUS offered: {uidplus}");
        assert_eq!(
            sent.search_message_id("Sent", &id("new")).await.unwrap(),
            vec![42]
        );
        assert_eq!(
            held(&server, "Sent")[1],
            (42, id("new"), vec!["\\Seen".to_string()])
        );
        assert_eq!(
            server.contents("Sent")[1].raw.as_deref(),
            Some(&mail("new")[..])
        );
        sent.close().await;
    }
}

/// The server's refusal of the APPEND is a refusal of the copy, which the
/// outbox gives a discarded row up on (PERSO-304), and nothing is filed.
#[tokio::test]
async fn a_refused_sent_append_is_the_servers_refusal() {
    let server = server(&[]);
    server.set_mail("Sent", 3, Vec::new());
    server.refuse("APPEND", Some("NO [OVERQUOTA] Mailbox is full"));
    let mut sent = super::sent::ImapSentMailbox::new(config(&server));
    let refused = sent
        .append("Sent", &mail("new"))
        .await
        .expect_err("refused");
    assert!(crate::outbox::is_sent_refusal(&refused), "{refused:#}");
    assert!(server.contents("Sent").is_empty());
}

/// An EXPUNGE reports each removed message by its number at the moment it
/// goes, the later ones renumbered by those before them (RFC 3501 7.4.1):
/// messages 2 and 3 of three are both reported as 2.
#[tokio::test]
async fn an_expunge_numbers_each_message_after_the_ones_before_it() {
    let server = server(&["one", "two", "three"]);
    server.set_flags("INBOX", 2, &["\\Deleted"]);
    server.set_flags("INBOX", 3, &["\\Deleted"]);
    let mut session = super::open_imap_session(&config(&server)).await.unwrap();
    session.select("INBOX").await.unwrap();
    let expunged = super::checked::Checked::expunge_checked(&mut session)
        .await
        .unwrap();
    assert_eq!(expunged, vec![2, 2]);
    let inbox: Vec<u32> = held(&server, "INBOX").into_iter().map(|m| m.0).collect();
    assert_eq!(inbox, vec![1]);
    session.logout().await.ok();
}

/// The refusal the tests below make the server give.
const LOCKED: &str = "NO [CANNOT] Mailbox is locked";

/// How a user reads [`LOCKED`], which is what every refused command's
/// error carries after the caller's own words.
const LOCKED_WORDS: &str = "the server answered NO: [CANNOT] Mailbox is locked";

/// A server's NO to the `UID STORE` of the read toggle and the flag changes
/// is the op's error in its words, never an empty success: the op is then
/// retried or rolled back instead of retired with the server unchanged.
#[tokio::test]
async fn a_refused_flag_store_is_the_ops_error() {
    let server = server(&["one"]);
    server.refuse("STORE", Some(LOCKED));
    let config = config(&server);
    let ops = [
        (
            super::ops::mark_read_on_server(&config, &id("one"), "INBOX", None).await,
            "+FLAGS (\\Seen)",
        ),
        (
            super::ops::mark_unread_on_server(&config, &id("one"), "INBOX", None).await,
            "-FLAGS (\\Seen)",
        ),
        (
            super::ops::add_flag_on_server(&config, &id("one"), "INBOX", None, "\\Flagged").await,
            "+FLAGS (\\Flagged)",
        ),
        (
            super::ops::remove_flag_on_server(&config, &id("one"), "INBOX", None, "\\Flagged")
                .await,
            "-FLAGS (\\Flagged)",
        ),
    ];
    for (result, change) in ops {
        let refused = result.expect_err(change);
        assert_eq!(
            format!("{refused:#}"),
            format!("Failed to store {change}: {LOCKED_WORDS}")
        );
    }
    assert!(held(&server, "INBOX")[0].2.is_empty(), "nothing changed");
}

/// A move whose `\Deleted` STORE or whose EXPUNGE the server refuses fails
/// in the server's words: the copy reached the destination, the source is
/// not gone, and the op is not done.
#[tokio::test]
async fn a_move_whose_store_or_expunge_is_refused_fails() {
    for (verb, uidplus, words) in [
        (
            "STORE",
            true,
            format!("Failed to mark email as deleted: {LOCKED_WORDS}"),
        ),
        (
            "EXPUNGE",
            true,
            format!("Failed to expunge 1: {LOCKED_WORDS}"),
        ),
        (
            "EXPUNGE",
            false,
            format!("Failed to expunge: {LOCKED_WORDS}"),
        ),
    ] {
        let server = server(&["one"]);
        server.set_uidplus(uidplus);
        server.refuse(verb, Some(LOCKED));
        let refused = super::ops::move_email_on_server(
            &config(&server),
            &id("one"),
            "INBOX",
            None,
            "Archive",
        )
        .await
        .expect_err(verb);
        assert_eq!(format!("{refused:#}"), words, "{verb}, UIDPLUS {uidplus}");
        assert_eq!(
            held(&server, "INBOX").len(),
            1,
            "{verb}: the source copy stays"
        );
    }
}

/// Gmail's tagged answer to a FETCH some of whose messages it could not
/// read, after the ones it could (RFC 2180 4.1.2).
const NOT_FETCHED: &str = "NO Some messages could not be FETCHed (Failure)";

/// How a user reads [`NOT_FETCHED`].
const NOT_FETCHED_WORDS: &str =
    "the server answered NO: Some messages could not be FETCHed (Failure)";

/// A body chunk the server answers `NO` after sending some of its messages
/// keeps the ones it sent: one message the server cannot read must not hold
/// back the rest of its chunk, pass after pass.
#[tokio::test]
async fn a_body_chunk_refused_for_one_message_keeps_the_others() {
    let server = server(&["one", "two", "three"]);
    server.refuse_fetch_of(2, Some(NOT_FETCHED));
    let mut session = super::open_imap_session(&config(&server)).await.unwrap();
    session.select("INBOX").await.unwrap();
    let fetched = super::fetch::fetch_body_chunk_on_session(&mut session, &[1, 2, 3])
        .await
        .expect("the messages the server sent");
    let uids: Vec<i64> = fetched.iter().map(|m| m.uid).collect();
    assert_eq!(uids, vec![1, 3]);
    session.logout().await.ok();
}

/// The search's second pass keeps the bodies the server sent and names the
/// one it refused, in its words, rather than as gone from the mailbox.
#[tokio::test]
async fn a_search_fetch_refused_for_one_message_keeps_the_others_and_says_why() {
    let server = server(&["one", "two", "three"]);
    server.refuse_fetch_of(2, Some(NOT_FETCHED));
    let mut session = super::open_imap_session(&config(&server)).await.unwrap();
    let fetched = super::fetch_uids_on_session(&mut session, "INBOX", &[1, 2, 3])
        .await
        .expect("the messages the server sent");
    let uids: Vec<u32> = fetched.emails.iter().map(|(uid, _)| *uid).collect();
    assert_eq!(uids, vec![1, 3]);
    assert_eq!(fetched.missing, vec![(2, NOT_FETCHED_WORDS.to_string())]);
    session.logout().await.ok();
}

/// A body download the server refuses outright names that refusal for
/// every message, instead of reporting them gone from the mailbox.
#[tokio::test]
async fn a_refused_body_fetch_names_the_refusal_not_an_empty_mailbox() {
    let server = server(&["one", "two"]);
    let mut session = super::open_imap_session(&config(&server)).await.unwrap();
    server.refuse("FETCH", Some(LOCKED));
    let fetched = super::fetch_uids_on_session(&mut session, "INBOX", &[1, 2])
        .await
        .expect("an answer");
    assert!(fetched.emails.is_empty());
    let why = LOCKED_WORDS.to_string();
    assert_eq!(fetched.missing, vec![(1, why.clone()), (2, why)]);
    session.logout().await.ok();
}

/// A fetch by Message-ID whose message the server refuses to send is that
/// refusal, not "no such message".
#[tokio::test]
async fn a_fetch_by_message_id_the_server_refuses_is_its_refusal() {
    let server = server(&["one", "two"]);
    server.refuse_fetch_of(2, Some(NOT_FETCHED));
    let mut session = super::open_imap_session(&config(&server)).await.unwrap();
    let refused = super::fetch_raw_by_message_id(&mut session, "INBOX", &id("two"))
        .await
        .expect_err("the server refused the message");
    assert_eq!(
        format!("{refused:#}"),
        format!("Failed to fetch email: {NOT_FETCHED_WORDS}")
    );
    let found = super::fetch_raw_by_message_id(&mut session, "INBOX", &id("one"))
        .await
        .unwrap()
        .expect("the other message");
    assert_eq!(found.0, 1);
    session.logout().await.ok();
}

/// A dedup search the server refuses is an error, never "no copy": the
/// drain would otherwise APPEND a second copy of a message the server may
/// hold. A busy server's refusal is no refusal of the copy (PERSO-304).
#[tokio::test]
async fn a_refused_sent_search_is_an_error_not_an_absent_copy() {
    let server = server(&[]);
    server.set_mail("Sent", 3, vec![(1, mail("filed"))]);
    server.refuse("SEARCH", Some("NO [UNAVAILABLE] Search is down"));
    let mut sent = super::sent::ImapSentMailbox::new(config(&server));
    let refused = sent
        .search_message_id("Sent", &id("filed"))
        .await
        .expect_err("the server refused the search");
    assert!(!crate::outbox::is_sent_refusal(&refused), "{refused:#}");
    assert_eq!(
        format!("{refused:#}"),
        "UID SEARCH in Sent failed: the server answered NO: [UNAVAILABLE] Search is down"
    );
}

/// A move whose copy search the server refuses fails in its words: the
/// copy was not found missing, so the op must not converge as done.
#[tokio::test]
async fn a_move_whose_search_is_refused_is_not_a_missing_copy() {
    let server = server(&["one"]);
    server.refuse("SEARCH", Some(LOCKED));
    let refused =
        super::ops::move_email_on_server(&config(&server), &id("one"), "INBOX", None, "Archive")
            .await
            .expect_err("the server refused the search");
    assert!(
        refused
            .downcast_ref::<crate::ops::NotFoundOnServer>()
            .is_none(),
        "{refused:#}"
    );
    assert_eq!(
        format!("{refused:#}"),
        format!("IMAP search failed: {LOCKED_WORDS}")
    );
    assert!(held(&server, "Archive").is_empty());
}

/// A LIST the server refuses is an error, not a server with no folders.
#[tokio::test]
async fn a_refused_list_is_an_error_not_an_empty_server() {
    let server = server(&["one"]);
    server.refuse("LIST", Some(LOCKED));
    let refused = super::list_mailboxes_detailed(&config(&server))
        .await
        .expect_err("the server refused the listing");
    assert_eq!(
        format!("{refused:#}"),
        format!("Failed to list mailboxes: {LOCKED_WORDS}")
    );
}

/// A pooled session whose `NOOP` probe the server refuses, or whose
/// connection ends before the probe's answer, is dropped and the checkout
/// logs in afresh, instead of handing out a session the server has given up
/// on.
#[tokio::test]
async fn a_pooled_session_whose_probe_fails_is_replaced_by_a_fresh_one() {
    use super::checked::Checked;
    for cut in [false, true] {
        let server = server(&["one"]);
        let config = config(&server);
        drop(
            super::pool::checkout(&config)
                .await
                .expect("a first session"),
        );
        let logins = server.logins();
        super::pool::age_idle_for_test(&config, std::time::Duration::from_secs(30));
        if cut {
            server.hang_up_on("NOOP", true);
        } else {
            server.refuse("NOOP", Some(LOCKED));
        }

        let mut fresh = super::pool::checkout(&config).await.expect("a session");
        assert_eq!(
            server.logins(),
            logins + 1,
            "cut: {cut}: the checkout logged in again"
        );
        server.hang_up_on("NOOP", false);
        server.refuse("NOOP", None);
        fresh
            .session()
            .noop_checked()
            .await
            .expect("the fresh session answers");
    }
}
