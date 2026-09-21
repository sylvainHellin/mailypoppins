//! The store half of iMIP invite reconciliation (#0030, moved onto rows and
//! blobs by [#0038](../docs/tickets/0038-read-path-to-db.md) scope item 6).
//!
//! Three readers: [`load_invites`] parses every `invite.ics` blob of one
//! account, [`event_for_message`] folds one row's card, and
//! [`reconcile_account`] reports what the fold resolves for `mp calendar
//! rebuild`. The fold itself reads nothing and lives in
//! [`mp_core::reconcile`](mp_core::reconcile), which this module re-exports
//! whole (#0126, P5-U10b): `crate::reconcile::fold_replies` and every other old
//! path resolve unchanged.

pub use mp_core::reconcile::*;

use crate::store::read::{self, MessageRow};
use crate::store::{BlobStore, Store};
use crate::types::EventFrontmatter;

/// Every invite of one account, parsed, in `(mailbox, uid)` order.
///
/// Rows whose ics blob is unreadable (retention evicted it) or unparseable are
/// skipped rather than failing the pass: one bad payload must not empty an
/// agenda. A store that cannot be queried yields an empty list, which is what
/// an account that has never synced looks like anyway.
pub fn load_invites(store: &Store, blobs: &BlobStore, account: &str) -> Vec<InviteMessage> {
    let rows = match read::list_invites(store, account) {
        Ok(rows) => rows,
        Err(e) => {
            log::warn!("[reconcile] listing invites for {account} failed: {e:#}");
            return Vec::new();
        }
    };
    rows.into_iter()
        .filter_map(|(row, hash)| invite_from_row(blobs, row, &hash))
        .collect()
}

/// Parse one invite row's ics blob into an [`InviteMessage`].
fn invite_from_row(blobs: &BlobStore, row: MessageRow, hash: &str) -> Option<InviteMessage> {
    let bytes = read::read_blob(blobs, row.id, hash)?;
    let parsed = crate::calendar::parse_ics(&bytes)?;
    Some(InviteMessage {
        row_id: row.id,
        mailbox: row.mailbox,
        uid: row.uid,
        subject: row.subject,
        parsed,
    })
}

/// The invitation card of one stored message: its own iMIP payload, with the
/// account's REPLY rows and cancellation chain folded onto it.
///
/// The primitive behind `message.invite` (P5-U10) and, before it, behind the
/// TUI's `App::load_message_invite`. It lives here rather than in either caller
/// because the fold is this module's: cancellation and supersession are
/// account-wide facts, not facts of one payload (#0031), so the card shows the
/// state of the event this message is a copy of, not the state the copy was
/// born with.
///
/// `None` when the row carries no iMIP payload or the payload does not parse,
/// which is what a non-invite looks like.
pub fn event_for_message(
    store: &Store,
    blobs: &BlobStore,
    account: &str,
    row_id: i64,
    self_address: &str,
) -> Option<EventFrontmatter> {
    let ics = read::load_invite_ics(store, blobs, row_id)?;
    let parsed = crate::calendar::parse_ics(&ics)?;
    let mut event = crate::calendar::event_frontmatter(&parsed);
    let uid = parsed
        .uid
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(str::to_string);
    let invites = load_invites(store, blobs, account);
    let replies = fold_replies(&invites);
    let by_addr = uid.as_deref().and_then(|uid| replies.get(uid));
    apply_replies(&mut event, parsed.sequence, by_addr);
    event.rsvp = own_rsvp(&event, self_address, by_addr);
    fold_status(&invites).apply(&mut event, parsed.dtstamp.as_deref().unwrap_or_default());
    Some(event)
}

/// Reconcile every invite of one account and report what the fold resolved.
///
/// The primitive behind `mp calendar rebuild`. It is a read: running it twice
/// reports the same numbers and changes nothing, because there is nothing to
/// change.
pub fn reconcile_account(store: &Store, blobs: &BlobStore, account: &str) -> ReconcileReport {
    let invites = load_invites(store, blobs, account);
    let replies = fold_replies(&invites);
    let status = fold_status(&invites);
    let mut report = ReconcileReport {
        replies_seen: invites.iter().filter(|i| i.method() == "REPLY").count(),
        ..Default::default()
    };
    for invite in invites.iter().filter(|i| i.method() == "REQUEST") {
        report.invites_seen += 1;
        let mut event = crate::calendar::event_frontmatter(&invite.parsed);
        let by_addr = invite.uid().and_then(|uid| replies.get(uid));
        report.resolved += apply_replies(&mut event, invite.parsed.sequence, by_addr);
        status.apply(&mut event, invite.dtstamp());
        if event.cancelled {
            report.cancelled += 1;
        }
    }
    report
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ingest::{ingest_message, IngestInput};
    use crate::parse::FetchedEmail;
    use tempfile::TempDir;

    /// A store plus its blob store under one temp directory, with the real
    /// ingest path as the only writer, so the fixture rows are the rows sync
    /// writes. Shared with the calendar loader's tests, which need the same
    /// "ingest an invite" primitive.
    pub(crate) struct Fixture {
        _dir: TempDir,
        pub(crate) store: Store,
        pub(crate) blobs: BlobStore,
    }

    pub(crate) fn fixture() -> Fixture {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        let blobs = BlobStore::new(dir.path().join("blobs"));
        Fixture {
            _dir: dir,
            store,
            blobs,
        }
    }

    impl Fixture {
        /// Ingest one message carrying `ics` into `mailbox`; returns its row id.
        pub(crate) fn ingest_invite(
            &self,
            mailbox: &str,
            uid: i64,
            subject: &str,
            ics: &str,
        ) -> i64 {
            self.ingest(mailbox, uid, subject, Some(ics))
        }

        /// Ingest one ordinary message, with no iMIP payload at all.
        pub(crate) fn ingest_plain(&self, mailbox: &str, uid: i64, subject: &str) -> i64 {
            self.ingest(mailbox, uid, subject, None)
        }

        /// Ingest one ordinary message carrying a named attachment, so a test
        /// can hand the store sender-controlled bytes (TKT-0047).
        pub(crate) fn ingest_with_attachment(
            &self,
            mailbox: &str,
            uid: i64,
            subject: &str,
            filename: &str,
            content: &[u8],
        ) -> i64 {
            let mut email = self.email(mailbox, uid, subject, None);
            email.has_attachments = true;
            email.attachments = vec![crate::parse::AttachmentData {
                filename: filename.to_string(),
                content: content.to_vec(),
                content_id: None,
            }];
            self.ingest_email(mailbox, uid, &email)
        }

        fn ingest(&self, mailbox: &str, uid: i64, subject: &str, ics: Option<&str>) -> i64 {
            let email = self.email(mailbox, uid, subject, ics);
            self.ingest_email(mailbox, uid, &email)
        }

        fn ingest_email(&self, mailbox: &str, uid: i64, email: &FetchedEmail) -> i64 {
            ingest_message(
                &self.store,
                &self.blobs,
                &IngestInput {
                    account: "alice",
                    mailbox,
                    uid,
                    email,
                    raw: None,
                },
            )
            .unwrap()
            .row_id
        }

        fn email(
            &self,
            mailbox: &str,
            uid: i64,
            subject: &str,
            ics: Option<&str>,
        ) -> FetchedEmail {
            FetchedEmail {
                from: "Organizer <me@example.com>".into(),
                to: "a@example.com".into(),
                cc: None,
                reply_to: None,
                bcc: None,
                subject: subject.into(),
                date: "Mon, 20 Jul 2026 09:00:00 +0000".into(),
                body_text: "You are invited.".into(),
                html_body: None,
                has_attachments: false,
                message_id: Some(format!("<{mailbox}-{uid}@example.com>")),
                attachments: Vec::new(),
                flags: Default::default(),
                calendar_ics: ics.map(|s| s.as_bytes().to_vec()),
                event: None,
            }
        }
    }

    /// The same primitive over the **ambient** data root rather than a private
    /// tempdir: a store at [`crate::config::store_path`] and blobs at
    /// [`crate::config::blobs_dir`], which is where a daemon looks for them.
    ///
    /// It lives here, beside the fold it seeds, so that the TUI's own
    /// daemon-versus-store equality rows (`src/tui/app/invites_tests.rs`) can
    /// have a seeded store and the two store-backed oracles without importing
    /// `crate::store` or `crate::ingest` under `src/tui/`: that import set is
    /// the allow-list `tests/architecture_boundaries.rs` drives to zero, and a
    /// test module widening it would be the boundary widening (#0124, P5-U10).
    pub(crate) struct AmbientFixture {
        _data: crate::config::test_env::TestDataDir,
        account: String,
        store: Store,
        blobs: BlobStore,
    }

    impl AmbientFixture {
        /// A fresh data root with an empty store for `account`. The store file
        /// is created here because it is what makes the account `ready` to a
        /// daemon; rows are seeded afterwards, which the per-call probe sees.
        pub(crate) fn new(account: &str) -> AmbientFixture {
            let data = crate::config::test_env::TestDataDir::new();
            let store = Store::open(crate::config::store_path(account)).expect("a store");
            let blobs = BlobStore::for_account(account);
            AmbientFixture {
                _data: data,
                account: account.to_string(),
                store,
                blobs,
            }
        }

        /// Ingest one message with `ics` as its iMIP payload, through the real
        /// ingest path; returns its `messages.id`.
        pub(crate) fn ingest(
            &self,
            mailbox: &str,
            uid: i64,
            subject: &str,
            ics: Option<&str>,
        ) -> i64 {
            let email = FetchedEmail {
                from: "Organizer <me@example.com>".into(),
                to: format!("{}@example.com", self.account),
                cc: None,
                reply_to: None,
                bcc: None,
                subject: subject.into(),
                date: "Mon, 20 Jul 2026 09:00:00 +0000".into(),
                body_text: "You are invited.".into(),
                html_body: None,
                has_attachments: false,
                message_id: Some(format!("<{mailbox}-{uid}@example.com>")),
                attachments: Vec::new(),
                flags: Default::default(),
                calendar_ics: ics.map(|s| s.as_bytes().to_vec()),
                event: None,
            };
            ingest_message(
                &self.store,
                &self.blobs,
                &IngestInput {
                    account: &self.account,
                    mailbox,
                    uid,
                    email: &email,
                    raw: None,
                },
            )
            .expect("the ingest")
            .row_id
        }

        /// The store-backed agenda, the oracle `calendar.events` answers.
        pub(crate) fn agenda(
            &self,
            self_address: &str,
        ) -> Vec<mp_protocol::calendar::AgendaEvent> {
            crate::agenda::load_events_for_account(
                &self.store,
                &self.blobs,
                &self.account,
                self_address,
            )
        }

        /// The store-backed invitation card, the oracle `message.invite`
        /// answers.
        pub(crate) fn card(&self, row_id: i64, self_address: &str) -> Option<EventFrontmatter> {
            event_for_message(
                &self.store,
                &self.blobs,
                &self.account,
                row_id,
                self_address,
            )
        }

        /// The store-backed `invite.ics` blob, the oracle `message.ics`
        /// answers.
        pub(crate) fn ics(&self, row_id: i64) -> Option<Vec<u8>> {
            read::load_invite_ics(&self.store, &self.blobs, row_id)
        }
    }

    /// A `METHOD:REQUEST` payload with `NEEDS-ACTION` attendees.
    pub(crate) fn invite_ics(uid: &str, seq: u32, attendees: &[&str]) -> String {
        let mut s = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:{uid}\r\n\
             SEQUENCE:{seq}\r\nSUMMARY:Plan\r\nDTSTART:20260720T120000Z\r\n\
             ORGANIZER:mailto:me@example.com\r\n"
        );
        for addr in attendees {
            s.push_str(&format!("ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:{addr}\r\n"));
        }
        s.push_str("END:VEVENT\r\nEND:VCALENDAR\r\n");
        s
    }

    /// A `METHOD:REPLY` payload carrying one attendee's answer.
    pub(crate) fn reply_ics(
        uid: &str,
        seq: u32,
        addr: &str,
        partstat: &str,
        dtstamp: &str,
    ) -> String {
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REPLY\r\nBEGIN:VEVENT\r\nUID:{uid}\r\n\
             SEQUENCE:{seq}\r\nDTSTAMP:{dtstamp}\r\nORGANIZER:mailto:me@example.com\r\n\
             ATTENDEE;PARTSTAT={partstat}:mailto:{addr}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        )
    }

    /// Fold the store's replies onto one invite and return its attendee list.
    fn statuses(fx: &Fixture, uid: &str) -> Vec<(String, String)> {
        let invites = load_invites(&fx.store, &fx.blobs, "alice");
        let replies = fold_replies(&invites);
        let request = invites
            .iter()
            .find(|i| i.method() == "REQUEST" && i.uid() == Some(uid))
            .expect("the REQUEST is in the store");
        let mut event = crate::calendar::event_frontmatter(&request.parsed);
        apply_replies(&mut event, request.parsed.sequence, replies.get(uid));
        event
            .attendees
            .into_iter()
            .map(|a| (a.address, a.status))
            .collect()
    }

    #[test]
    fn a_reply_flips_the_matching_attendee() {
        let fx = fixture();
        fx.ingest_invite("sent", 1, "Plan", &invite_ics("u1@x", 0, &["a@example.com"]));
        fx.ingest_invite(
            "inbox",
            2,
            "Re: Plan",
            &reply_ics("u1@x", 0, "a@example.com", "ACCEPTED", "20260710T120000Z"),
        );
        assert_eq!(
            statuses(&fx, "u1@x"),
            vec![("a@example.com".to_string(), "accepted".to_string())]
        );
    }

    #[test]
    fn addresses_match_case_insensitively() {
        let fx = fixture();
        fx.ingest_invite(
            "sent",
            1,
            "Plan",
            &invite_ics("u1@x", 0, &["Alice@Example.com"]),
        );
        fx.ingest_invite(
            "inbox",
            2,
            "Re: Plan",
            &reply_ics("u1@x", 0, "alice@example.com", "DECLINED", "20260710T120000Z"),
        );
        assert_eq!(statuses(&fx, "u1@x")[0].1, "declined");
    }

    #[test]
    fn the_latest_dtstamp_wins_within_a_sequence() {
        let fx = fixture();
        fx.ingest_invite("sent", 1, "Plan", &invite_ics("u1@x", 0, &["a@example.com"]));
        fx.ingest_invite(
            "inbox",
            2,
            "Re: Plan",
            &reply_ics("u1@x", 0, "a@example.com", "ACCEPTED", "20260710T090000Z"),
        );
        fx.ingest_invite(
            "inbox",
            3,
            "Re: Plan",
            &reply_ics("u1@x", 0, "a@example.com", "DECLINED", "20260711T090000Z"),
        );
        assert_eq!(statuses(&fx, "u1@x")[0].1, "declined");
    }

    #[test]
    fn a_newer_sequence_reply_wins_and_an_older_one_is_ignored() {
        let fx = fixture();
        // The invite was bumped to sequence 2, so a reply for sequence 1
        // answered a version of the event that no longer exists.
        fx.ingest_invite("sent", 1, "Plan", &invite_ics("u1@x", 2, &["a@example.com"]));
        fx.ingest_invite(
            "inbox",
            2,
            "Re: Plan",
            &reply_ics("u1@x", 1, "a@example.com", "ACCEPTED", "20260710T120000Z"),
        );
        assert_eq!(statuses(&fx, "u1@x")[0].1, "needs-action");

        fx.ingest_invite(
            "inbox",
            3,
            "Re: Plan",
            &reply_ics("u1@x", 3, "a@example.com", "TENTATIVE", "20260709T090000Z"),
        );
        assert_eq!(statuses(&fx, "u1@x")[0].1, "tentative");
    }

    #[test]
    fn a_reply_from_an_uninvited_address_is_ignored() {
        let fx = fixture();
        fx.ingest_invite("sent", 1, "Plan", &invite_ics("u1@x", 0, &["a@example.com"]));
        fx.ingest_invite(
            "inbox",
            2,
            "Re: Plan",
            &reply_ics(
                "u1@x",
                0,
                "stranger@example.com",
                "ACCEPTED",
                "20260710T120000Z",
            ),
        );
        assert_eq!(statuses(&fx, "u1@x")[0].1, "needs-action");
    }

    /// The report counts what it saw and resolved, and a second pass reports
    /// the same numbers: with nothing written there is nothing to converge.
    #[test]
    fn the_report_is_stable_across_passes() {
        let fx = fixture();
        fx.ingest_invite(
            "sent",
            1,
            "Plan",
            &invite_ics("u1@x", 0, &["a@example.com", "b@example.com"]),
        );
        fx.ingest_invite(
            "inbox",
            2,
            "Re: Plan",
            &reply_ics("u1@x", 0, "a@example.com", "ACCEPTED", "20260710T120000Z"),
        );
        fx.ingest_invite(
            "inbox",
            3,
            "Re: Plan",
            &reply_ics("u1@x", 0, "b@example.com", "DECLINED", "20260710T120000Z"),
        );

        let first = reconcile_account(&fx.store, &fx.blobs, "alice");
        assert_eq!(
            first,
            ReconcileReport {
                invites_seen: 1,
                replies_seen: 2,
                resolved: 2,
                cancelled: 0,
            }
        );
        assert_eq!(
            reconcile_account(&fx.store, &fx.blobs, "alice"),
            first,
            "a second pass must report the same numbers"
        );
    }

    /// Our own answer comes from our own sent REPLY, which the outbox ingests
    /// during the send, so the invite shows it without waiting for a sync.
    #[test]
    fn our_own_rsvp_comes_from_our_own_reply() {
        let fx = fixture();
        fx.ingest_invite(
            "inbox",
            1,
            "Plan",
            &invite_ics("u1@x", 0, &["me@example.com"]),
        );
        let invites = load_invites(&fx.store, &fx.blobs, "alice");
        let request = invites.iter().find(|i| i.method() == "REQUEST").unwrap();
        let event = crate::calendar::event_frontmatter(&request.parsed);
        assert_eq!(
            own_rsvp(&event, "me@example.com", None),
            "needs-action",
            "before we answer, the organizer's PARTSTAT for us stands"
        );

        fx.ingest_invite(
            "sent",
            2,
            "Declined: Plan",
            &reply_ics("u1@x", 0, "me@example.com", "DECLINED", "20260710T120000Z"),
        );
        let invites = load_invites(&fx.store, &fx.blobs, "alice");
        let replies = fold_replies(&invites);
        assert_eq!(
            own_rsvp(&event, "Me@Example.com", replies.get("u1@x")),
            "declined"
        );
    }

    /// An unreadable or unparseable ics costs its own row and nothing else.
    #[test]
    fn an_unreadable_ics_skips_only_that_invite() {
        let fx = fixture();
        let broken = fx.ingest_invite("inbox", 1, "Broken", "not an ics at all");
        fx.ingest_invite("inbox", 2, "Plan", &invite_ics("u1@x", 0, &["a@example.com"]));

        let invites = load_invites(&fx.store, &fx.blobs, "alice");
        assert_eq!(invites.len(), 1, "the unparseable payload is skipped");
        assert_ne!(invites[0].row_id, broken);
        assert_eq!(invites[0].uid(), Some("u1@x"));
    }

    /// TKT-0047, closed by construction in [#0040]: a sender-controlled `.md`
    /// attachment carrying a forged `method: REPLY` used to be walked by
    /// `build_index` and written into a real invite's `PARTSTAT` on disk.
    /// There is no walk and no frontmatter writer left: the fold reads
    /// `invite.ics` blobs of message rows, and an attachment blob is not a
    /// row. This pins that, so the surface cannot come back unnoticed.
    ///
    /// [#0040]: ../docs/tickets/0040-drop-file-layer-cutover.md
    #[test]
    fn a_forged_md_attachment_cannot_move_a_partstat() {
        let fx = fixture();
        fx.ingest_invite("sent", 1, "Plan", &invite_ics("u1@x", 0, &["a@example.com"]));

        // The exact shape the old walk classified: frontmatter with from/to/
        // subject and an event: block, method REPLY, the real UID, and a
        // sequence/dtstamp that would win every tiebreak.
        let forged = b"---\nfrom: attacker@evil.example\nto: me@example.com\n\
subject: invoice\nmethod: REPLY\nevent:\n  uid: u1@x\n  sequence: 99\n  \
dtstamp: 20991231T235959Z\n  attendees:\n    - address: a@example.com\n      \
status: accepted\n---\n\nsee attached\n";
        fx.ingest_with_attachment("inbox", 2, "invoice", "forged.md", forged);

        assert_eq!(
            statuses(&fx, "u1@x"),
            vec![("a@example.com".to_string(), "needs-action".to_string())],
            "an attachment blob must not reach the fold"
        );
        assert_eq!(
            reconcile_account(&fx.store, &fx.blobs, "alice").replies_seen,
            0,
            "the forged attachment is not a reply"
        );
    }

    /// A `METHOD:CANCEL` payload for a whole series, or (with `recurrence_id`)
    /// for one occurrence of it.
    fn cancel_ics(uid: &str, seq: u32, recurrence_id: Option<&str>) -> String {
        let rid = recurrence_id
            .map(|r| format!("RECURRENCE-ID:{r}\r\n"))
            .unwrap_or_default();
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:CANCEL\r\nBEGIN:VEVENT\r\nUID:{uid}\r\n\
             SEQUENCE:{seq}\r\nDTSTAMP:20260715T090000Z\r\nSTATUS:CANCELLED\r\n{rid}\
             ORGANIZER:mailto:me@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        )
    }

    /// A `METHOD:REQUEST` payload with an explicit `DTSTAMP` and an optional
    /// `RECURRENCE-ID` (an occurrence override).
    fn request_ics(uid: &str, seq: u32, dtstamp: &str, recurrence_id: Option<&str>) -> String {
        let rid = recurrence_id
            .map(|r| format!("RECURRENCE-ID:{r}\r\n"))
            .unwrap_or_default();
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:{uid}\r\n\
             SEQUENCE:{seq}\r\nDTSTAMP:{dtstamp}\r\nSUMMARY:Plan\r\nDTSTART:20260720T120000Z\r\n\
             RRULE:FREQ=WEEKLY\r\n{rid}ORGANIZER:mailto:me@example.com\r\n\
             ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:a@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        )
    }

    /// The folded state of the stored REQUEST matching `uid`/`recurrence_id`.
    fn folded(fx: &Fixture, uid: &str, recurrence_id: Option<&str>) -> EventFrontmatter {
        let invites = load_invites(&fx.store, &fx.blobs, "alice");
        let status = fold_status(&invites);
        let request = invites
            .iter()
            .find(|i| {
                i.method() == "REQUEST"
                    && i.uid() == Some(uid)
                    && i.recurrence_id() == recurrence_id
            })
            .expect("the REQUEST is in the store");
        let mut event = crate::calendar::event_frontmatter(&request.parsed);
        status.apply(&mut event, request.dtstamp());
        event
    }

    /// A whole-series CANCEL tombstones the event: it is marked cancelled and
    /// still readable, never removed.
    #[test]
    fn a_cancel_tombstones_the_whole_event() {
        let fx = fixture();
        fx.ingest_invite("inbox", 1, "Plan", &request_ics("u1@x", 0, "20260701T090000Z", None));
        fx.ingest_invite("inbox", 2, "Cancelled: Plan", &cancel_ics("u1@x", 1, None));
        let event = folded(&fx, "u1@x", None);
        assert!(event.cancelled);
        assert!(event.cancelled_instances.is_empty());
        assert_eq!(event.summary.as_deref(), Some("Plan"), "the event is kept");
        assert_eq!(
            reconcile_account(&fx.store, &fx.blobs, "alice").cancelled,
            1
        );
    }

    /// Arrival order is irrelevant: a CANCEL ingested *before* its REQUEST
    /// tombstones it just as one ingested after would.
    #[test]
    fn a_cancel_that_arrives_before_its_request_still_applies() {
        let fx = fixture();
        fx.ingest_invite("inbox", 1, "Cancelled: Plan", &cancel_ics("u2@x", 1, None));
        fx.ingest_invite("inbox", 2, "Plan", &request_ics("u2@x", 0, "20260701T090000Z", None));
        assert!(folded(&fx, "u2@x", None).cancelled);
    }

    /// A CANCEL naming one `RECURRENCE-ID` kills that occurrence only: the
    /// series stays live and lists the cancelled occurrence.
    #[test]
    fn an_occurrence_cancel_does_not_kill_the_series() {
        let fx = fixture();
        fx.ingest_invite("inbox", 1, "Plan", &request_ics("u3@x", 0, "20260701T090000Z", None));
        fx.ingest_invite(
            "inbox",
            2,
            "Cancelled: Plan",
            &cancel_ics("u3@x", 1, Some("20260727T120000Z")),
        );
        let series = folded(&fx, "u3@x", None);
        assert!(!series.cancelled, "the series survives an occurrence cancel");
        assert_eq!(
            series.cancelled_instances,
            vec!["2026-07-27T12:00:00Z".to_string()]
        );
    }

    /// The occurrence override itself is what the occurrence CANCEL cancels,
    /// and the series row beside it is untouched.
    #[test]
    fn an_occurrence_cancel_marks_only_that_occurrence() {
        let fx = fixture();
        fx.ingest_invite("inbox", 1, "Plan", &request_ics("u4@x", 0, "20260701T090000Z", None));
        fx.ingest_invite(
            "inbox",
            2,
            "Plan (moved)",
            &request_ics("u4@x", 0, "20260702T090000Z", Some("20260727T120000Z")),
        );
        fx.ingest_invite(
            "inbox",
            3,
            "Cancelled: Plan",
            &cancel_ics("u4@x", 0, Some("20260727T120000Z")),
        );
        assert!(folded(&fx, "u4@x", Some("2026-07-27T12:00:00Z")).cancelled);
        assert!(!folded(&fx, "u4@x", None).cancelled);
    }

    /// A stale CANCEL (a lower sequence than the surviving REQUEST) cancelled
    /// a version that has already been replaced, and must not tombstone the
    /// rescheduled event.
    #[test]
    fn a_stale_cancel_does_not_tombstone_a_newer_request() {
        let fx = fixture();
        fx.ingest_invite("inbox", 1, "Cancelled: Plan", &cancel_ics("u5@x", 0, None));
        fx.ingest_invite("inbox", 2, "Plan", &request_ics("u5@x", 2, "20260701T090000Z", None));
        assert!(!folded(&fx, "u5@x", None).cancelled);
    }

    /// A UID-less CANCEL has no identity to apply to and must never tombstone
    /// an unrelated event.
    #[test]
    fn a_uidless_cancel_tombstones_nothing() {
        let fx = fixture();
        fx.ingest_invite("inbox", 1, "Plan", &request_ics("u6@x", 0, "20260701T090000Z", None));
        fx.ingest_invite(
            "inbox",
            2,
            "Cancelled",
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:CANCEL\r\nBEGIN:VEVENT\r\n\
             SEQUENCE:9\r\nDTSTAMP:20260715T090000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        );
        assert!(!folded(&fx, "u6@x", None).cancelled);
    }

    /// A malformed CANCEL is skipped like any other unparseable payload: the
    /// event stays live, the other invites are unaffected, and the pass does
    /// not fail.
    #[test]
    fn a_malformed_cancel_degrades_to_no_cancellation() {
        let fx = fixture();
        fx.ingest_invite("inbox", 1, "Plan", &request_ics("u7@x", 0, "20260701T090000Z", None));
        fx.ingest_invite(
            "inbox",
            2,
            "Cancelled: Plan",
            "BEGIN:VCALENDAR\r\nMETHOD:CANCEL\r\nthis is not an ics\r\n",
        );
        assert!(!folded(&fx, "u7@x", None).cancelled);
        assert_eq!(
            reconcile_account(&fx.store, &fx.blobs, "alice"),
            ReconcileReport {
                invites_seen: 1,
                replies_seen: 0,
                resolved: 0,
                cancelled: 0,
            }
        );
    }

    /// A re-issue with a higher `SEQUENCE` supersedes the copy already stored;
    /// the new copy is not itself superseded.
    #[test]
    fn a_higher_sequence_request_supersedes_the_stored_one() {
        let fx = fixture();
        fx.ingest_invite("inbox", 1, "Plan", &request_ics("u8@x", 0, "20260701T090000Z", None));
        fx.ingest_invite("inbox", 2, "Plan (moved)", &request_ics("u8@x", 1, "20260702T090000Z", None));
        let invites = load_invites(&fx.store, &fx.blobs, "alice");
        let status = fold_status(&invites);
        for invite in invites.iter().filter(|i| i.method() == "REQUEST") {
            let mut event = crate::calendar::event_frontmatter(&invite.parsed);
            status.apply(&mut event, invite.dtstamp());
            assert_eq!(
                event.superseded,
                event.sequence == 0,
                "sequence {} superseded={}",
                event.sequence,
                event.superseded
            );
        }
    }

    /// The clobber guard: a re-delivered copy at a lower *or equal*
    /// `(SEQUENCE, DTSTAMP)` never marks the newer state as superseded, so a
    /// replayed old invite cannot displace what is stored.
    #[test]
    fn a_stale_or_equal_sequence_request_never_supersedes() {
        let fx = fixture();
        // sequence 2 is the current version.
        fx.ingest_invite("inbox", 1, "Plan", &request_ics("u9@x", 2, "20260703T090000Z", None));
        // A replay of sequence 1, and a duplicate of sequence 2 with the same
        // DTSTAMP (the same version delivered twice).
        fx.ingest_invite("archive", 2, "Plan", &request_ics("u9@x", 1, "20260705T090000Z", None));
        fx.ingest_invite("archive", 3, "Plan", &request_ics("u9@x", 2, "20260703T090000Z", None));

        let invites = load_invites(&fx.store, &fx.blobs, "alice");
        let status = fold_status(&invites);
        let current = invites
            .iter()
            .find(|i| i.parsed.sequence == 2 && i.mailbox == "inbox")
            .unwrap();
        let mut event = crate::calendar::event_frontmatter(&current.parsed);
        status.apply(&mut event, current.dtstamp());
        assert!(
            !event.superseded,
            "a lower/equal-version copy must not supersede the stored one"
        );
    }

    /// An account with no invites reconciles to an empty report, not an error.
    #[test]
    fn an_account_with_no_invites_reports_nothing() {
        let fx = fixture();
        assert_eq!(
            reconcile_account(&fx.store, &fx.blobs, "alice"),
            ReconcileReport::default()
        );
    }
}
