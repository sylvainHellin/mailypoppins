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
        "Failed to copy email to Nowhere: no response: code: Some(TryCreate), \
         info: Some(\"Mailbox doesn't exist: Nowhere\")",
        "async-imap's rendering of the server's answer, which is what the op's error shows"
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
    use futures::TryStreamExt;
    let server = server(&["one", "two", "three"]);
    server.set_flags("INBOX", 2, &["\\Deleted"]);
    server.set_flags("INBOX", 3, &["\\Deleted"]);
    let mut session = super::open_imap_session(&config(&server)).await.unwrap();
    session.select("INBOX").await.unwrap();
    let expunged: Vec<u32> = session
        .expunge()
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(expunged, vec![2, 2]);
    let inbox: Vec<u32> = held(&server, "INBOX").into_iter().map(|m| m.0).collect();
    assert_eq!(inbox, vec![1]);
    session.logout().await.ok();
}
