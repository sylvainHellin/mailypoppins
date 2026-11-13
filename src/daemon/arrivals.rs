//! Which of a tick's new inbox messages are worth a notification (PERSO-186).
//!
//! The engine announces what a pass inserted above what the account held
//! before it ([`crate::sync::SyncResult::new_inbox_mail`]), which already
//! leaves out a first sync, a backlog drain and a renumbered mailbox. What it
//! cannot know is what happened to a message after it was written: the
//! blocked senders and the mail hooks run once the tick is over and may move
//! it to Junk, and the user may read it in another client before the daemon
//! gets to it. [`worth_notifying`] reads every announced row again once those
//! judges are done ([`crate::daemon::runtime::arrival_runner`]) and keeps the
//! messages that are still unread in the Inbox, from neither a blocked sender
//! nor the account's own address. A client only presents what survives.

use anyhow::Result;

use mp_protocol::events::Arrival;

use crate::config::AccountConfig;
use crate::store::{read, Store};
use crate::sync::InboxArrival;

/// The announced `candidates` still worth a notification, read from `store`
/// now, in the order they were announced; `blocked` is the blocked-senders
/// list ([`crate::daemon::senders::load`]).
pub fn worth_notifying(
    store: &Store,
    account: &AccountConfig,
    blocked: &[String],
    candidates: &[InboxArrival],
) -> Result<Vec<Arrival>> {
    let inbox = crate::types::MailboxRole::Inbox.as_str();
    let mut worth = Vec::new();
    for candidate in candidates {
        let Some(row) = read::find_by_id(store, candidate.row_id)? else {
            continue;
        };
        if row.message_id != candidate.message_id || row.mailbox != inbox || row.is_read() {
            continue;
        }
        let from = row.from.unwrap_or_default();
        if account.sends_as(&from) || crate::daemon::senders::matches(blocked, &from) {
            continue;
        }
        worth.push(Arrival {
            from,
            subject: row.subject.unwrap_or_default(),
        });
    }
    Ok(worth)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::{ingest_message, IngestInput};
    use crate::store::BlobStore;

    struct Fixture {
        _tmp: tempfile::TempDir,
        store: Store,
        blobs: BlobStore,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("store.sqlite3")).unwrap();
        let blobs = BlobStore::new(tmp.path().join("blobs"));
        Fixture {
            _tmp: tmp,
            store,
            blobs,
        }
    }

    fn account() -> AccountConfig {
        AccountConfig {
            name: "acct".to_string(),
            default_from: "Me <me@example.com>".to_string(),
            ..Default::default()
        }
    }

    /// Ingest one Inbox message from `from`, announced the way the engine
    /// announces it.
    fn arrive(fx: &Fixture, uid: i64, from: &str, subject: &str) -> InboxArrival {
        let raw = format!(
            "From: {from}\r\nTo: me@example.com\r\nSubject: {subject}\r\n\
             Message-ID: <{uid}@example.com>\r\nDate: Wed, 30 Sep 2026 10:00:00 +0200\r\n\r\nbody\r\n"
        );
        let email = crate::parse::parse_rfc822_to_fetched_email(raw.as_bytes()).unwrap();
        let outcome = ingest_message(
            &fx.store,
            &fx.blobs,
            &IngestInput {
                account: "acct",
                mailbox: "inbox",
                uid,
                email: &email,
                raw: Some(raw.as_bytes()),
            },
        )
        .unwrap();
        InboxArrival::of(&outcome, &email)
    }

    fn set(fx: &Fixture, column: &str, value: &str, row: i64) {
        fx.store
            .conn()
            .execute(
                &format!("UPDATE messages SET {column} = ?1 WHERE id = ?2"),
                rusqlite::params![value, row],
            )
            .unwrap();
    }

    fn subjects(arrivals: &[Arrival]) -> Vec<&str> {
        arrivals.iter().map(|a| a.subject.as_str()).collect()
    }

    /// A message still unread in the Inbox from someone else is kept, with
    /// its sender and subject as the store holds them.
    #[test]
    fn unread_inbox_mail_from_someone_else_is_kept() {
        let fx = fixture();
        let one = arrive(&fx, 11, "Ada <ada@example.com>", "Engine");
        let kept = worth_notifying(&fx.store, &account(), &[], &[one]).unwrap();
        assert_eq!(
            kept,
            vec![Arrival {
                from: "Ada <ada@example.com>".to_string(),
                subject: "Engine".to_string(),
            }]
        );
    }

    /// What a judge or another client did after the tick wrote the row is
    /// what counts: a message moved to Junk (a hook, a block, the user), one
    /// read elsewhere, and one deleted are dropped, and so is a row whose id a
    /// later insert reused.
    #[test]
    fn mail_moved_read_or_gone_since_the_tick_is_dropped() {
        let fx = fixture();
        let junked = arrive(&fx, 11, "Spam <spam@example.net>", "junked");
        let read = arrive(&fx, 12, "Bob <bob@example.com>", "read");
        let gone = arrive(&fx, 13, "Cy <cy@example.com>", "gone");
        let reused = InboxArrival {
            message_id: "<another@example.com>".to_string(),
            ..arrive(&fx, 14, "Di <di@example.com>", "reused")
        };
        let kept = arrive(&fx, 15, "Eve <eve@example.com>", "kept");
        set(&fx, "mailbox", "junk", junked.row_id);
        set(&fx, "flags", "\\Seen", read.row_id);
        fx.store
            .conn()
            .execute("DELETE FROM messages WHERE id = ?1", [gone.row_id])
            .unwrap();

        let worth = worth_notifying(
            &fx.store,
            &account(),
            &[],
            &[junked, read, gone, reused, kept],
        )
        .unwrap();
        assert_eq!(subjects(&worth), vec!["kept"]);
    }

    /// The account's own copy (a message to itself, a Bcc, a provider that
    /// files sent mail in the Inbox) and a blocked sender's mail, which the
    /// scan could not move when the account has no Junk folder, are dropped.
    #[test]
    fn own_and_blocked_senders_are_dropped() {
        let fx = fixture();
        let own = arrive(&fx, 11, "ME@example.com", "own");
        let blocked = arrive(&fx, 12, "Promo <deals@shop.example>", "blocked");
        let kept = arrive(&fx, 13, "Ada <ada@example.com>", "kept");
        let worth = worth_notifying(
            &fx.store,
            &account(),
            &["@shop.example".to_string()],
            &[own, blocked, kept],
        )
        .unwrap();
        assert_eq!(subjects(&worth), vec!["kept"]);
    }
}
