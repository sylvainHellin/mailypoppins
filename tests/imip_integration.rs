//! End-to-end iMIP receive tests (ticket #0027): raw RFC822 MIME -> parse ->
//! store ingest. Verifies that `text/calendar` parts are classified as invites
//! or as ordinary attachments, and that the invite payload reaches the store as
//! the `invite.ics` attachment blob without a duplicate copy.
//!
//! Rewritten for #0037 unit 4a: the assertions used to read the `.md` file and
//! its sidecar, which no ingest writes any more, so they now read the
//! `message_blobs` rows and the blob bytes. The classification being asserted
//! is unchanged.

use mailypoppins::parse::{parse_rfc822_to_fetched_email, CALENDAR_SIDECAR_NAME};
use tempfile::tempdir;

/// Ingest one parsed message into a throwaway store and expose what the row
/// references, so the iMIP classification can be asserted on what actually
/// landed instead of on a `.md` file that is no longer written (#0037).
struct Ingested {
    _tmp: tempfile::TempDir,
    store: mailypoppins::store::Store,
    blobs: mailypoppins::store::BlobStore,
    row: i64,
}

fn ingest(email: &mailypoppins::parse::FetchedEmail, mailbox: &str) -> Ingested {
    let tmp = tempdir().unwrap();
    let store = mailypoppins::store::Store::open(tmp.path().join("store.sqlite3")).unwrap();
    let blobs = mailypoppins::store::BlobStore::new(tmp.path().join("blobs"));
    let outcome = mailypoppins::ingest::ingest_message(
        &store,
        &blobs,
        &mailypoppins::ingest::IngestInput {
            account: "acct",
            mailbox,
            uid: 1,
            email,
            raw: None,
        },
    )
    .unwrap();
    Ingested {
        _tmp: tmp,
        store,
        blobs,
        row: outcome.row_id,
    }
}

impl Ingested {
    /// Attachment blob filenames in ingest order.
    fn attachment_names(&self) -> Vec<String> {
        let mut stmt = self
            .store
            .conn()
            .prepare(
                "SELECT filename FROM message_blobs
                 WHERE message_row = ?1 AND kind = 'attachment' ORDER BY ordinal",
            )
            .unwrap();
        let rows = stmt
            .query_map([self.row], |r| r.get::<_, Option<String>>(0))
            .unwrap();
        rows.map(|r| r.unwrap().unwrap_or_default()).collect()
    }

    /// Bytes of the attachment blob stored under `filename`.
    fn attachment_bytes(&self, filename: &str) -> Option<Vec<u8>> {
        let hash: Option<String> = self
            .store
            .conn()
            .query_row(
                "SELECT hash FROM message_blobs
                 WHERE message_row = ?1 AND kind = 'attachment' AND filename = ?2",
                (self.row, filename),
                |r| r.get(0),
            )
            .ok();
        hash.map(|h| {
            self.blobs
                .read(&mailypoppins::store::blobs::BlobHash::parse(&h).unwrap())
                .unwrap()
        })
    }
}

// Outlook-style: multipart/alternative with an inline text/calendar REQUEST
// using a TZID (Europe/Berlin -> +02:00 in July).
const OUTLOOK_INLINE_REQUEST: &str = "\
From: Chair <chair@tum.de>\r
To: me@example.com\r
Subject: LOC Day planning\r
Date: Mon, 13 Jul 2026 09:00:00 +0000\r
Message-ID: <outlook-1@tum.de>\r
MIME-Version: 1.0\r
Content-Type: multipart/alternative; boundary=\"BOUND\"\r
\r
--BOUND\r
Content-Type: text/plain; charset=UTF-8\r
\r
You are invited to LOC Day planning.\r
--BOUND\r
Content-Type: text/calendar; method=REQUEST; charset=UTF-8\r
Content-Transfer-Encoding: 7bit\r
\r
BEGIN:VCALENDAR\r
PRODID:-//Microsoft Corporation//Outlook 16.0 MIMEDIR//EN\r
VERSION:2.0\r
METHOD:REQUEST\r
BEGIN:VEVENT\r
UID:outlook-uid-1@tum.de\r
SEQUENCE:2\r
SUMMARY:LOC Day planning\r
DTSTART;TZID=Europe/Berlin:20260720T140000\r
DTEND;TZID=Europe/Berlin:20260720T150000\r
LOCATION:Room 4.12\r
ORGANIZER;CN=Chair:mailto:chair@tum.de\r
ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:me@example.com\r
END:VEVENT\r
END:VCALENDAR\r
--BOUND--\r
";

// Google-style: multipart/mixed with a text/plain body plus a named
// text/calendar attachment (invite.ics), UTC times and an RRULE.
const GOOGLE_ICS_ATTACHMENT_REQUEST: &str = "\
From: Host <host@gmail.com>\r
To: me@example.com\r
Subject: Weekly sync\r
Date: Mon, 13 Jul 2026 09:00:00 +0000\r
Message-ID: <google-1@gmail.com>\r
MIME-Version: 1.0\r
Content-Type: multipart/mixed; boundary=\"MIX\"\r
\r
--MIX\r
Content-Type: text/plain; charset=UTF-8\r
\r
You have been invited to Weekly sync.\r
--MIX\r
Content-Type: text/calendar; charset=UTF-8; method=REQUEST; name=\"invite.ics\"\r
Content-Disposition: attachment; filename=\"invite.ics\"\r
\r
BEGIN:VCALENDAR\r
PRODID:-//Google Inc//Google Calendar 70.9054//EN\r
VERSION:2.0\r
METHOD:REQUEST\r
BEGIN:VEVENT\r
UID:google-uid-1@google.com\r
SEQUENCE:0\r
SUMMARY:Weekly sync\r
DTSTART:20260720T120000Z\r
DTEND:20260720T130000Z\r
RRULE:FREQ=WEEKLY;BYDAY=MO\r
ORGANIZER:mailto:host@gmail.com\r
ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:me@example.com\r
END:VEVENT\r
END:VCALENDAR\r
--MIX--\r
";

// Malformed calendar part: valid MIME, unparseable ICS body.
const MALFORMED_INVITE: &str = "\
From: Someone <someone@example.com>\r
To: me@example.com\r
Subject: Broken invite\r
Date: Mon, 13 Jul 2026 09:00:00 +0000\r
Message-ID: <malformed-1@example.com>\r
MIME-Version: 1.0\r
Content-Type: multipart/alternative; boundary=\"B\"\r
\r
--B\r
Content-Type: text/plain; charset=UTF-8\r
\r
Body text.\r
--B\r
Content-Type: text/calendar; method=REQUEST; charset=UTF-8\r
\r
this is not a valid vcalendar payload at all\r
--B--\r
";

// An iMIP invite (REQUEST) PLUS a second, user-shared calendar export attached
// as a separate `.ics` document. Only the invite should be lifted to the
// sidecar; the export must survive as a regular attachment with its filename.
const INVITE_PLUS_SHARED_ICS: &str = "\
From: Host <host@gmail.com>\r
To: me@example.com\r
Subject: Weekly sync + my calendar\r
Date: Mon, 13 Jul 2026 09:00:00 +0000\r
Message-ID: <invite-plus-1@gmail.com>\r
MIME-Version: 1.0\r
Content-Type: multipart/mixed; boundary=\"MIX\"\r
\r
--MIX\r
Content-Type: text/plain; charset=UTF-8\r
\r
You have been invited to Weekly sync. Here's my calendar too.\r
--MIX\r
Content-Type: text/calendar; charset=UTF-8; method=REQUEST; name=\"invite.ics\"\r
Content-Disposition: attachment; filename=\"invite.ics\"\r
\r
BEGIN:VCALENDAR\r
VERSION:2.0\r
METHOD:REQUEST\r
BEGIN:VEVENT\r
UID:invite-plus-uid@google.com\r
SEQUENCE:0\r
SUMMARY:Weekly sync\r
DTSTART:20260720T120000Z\r
DTEND:20260720T130000Z\r
ORGANIZER:mailto:host@gmail.com\r
ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:me@example.com\r
END:VEVENT\r
END:VCALENDAR\r
--MIX\r
Content-Type: application/octet-stream; name=\"my-calendar.ics\"\r
Content-Disposition: attachment; filename=\"my-calendar.ics\"\r
\r
BEGIN:VCALENDAR\r
VERSION:2.0\r
PRODID:-//Me//Export//EN\r
BEGIN:VEVENT\r
UID:my-export-1@example.com\r
SUMMARY:Dentist\r
DTSTART:20260801T090000Z\r
DTEND:20260801T093000Z\r
END:VEVENT\r
END:VCALENDAR\r
--MIX--\r
";

// A single non-iMIP `.ics` calendar export (no METHOD property). It is not an
// invite: it must be stored as a regular attachment, keeping its filename, with
// no sidecar and no event block.
const SHARED_ICS_EXPORT_ONLY: &str = "\
From: Friend <friend@example.com>\r
To: me@example.com\r
Subject: My calendar export\r
Date: Mon, 13 Jul 2026 09:00:00 +0000\r
Message-ID: <export-only-1@example.com>\r
MIME-Version: 1.0\r
Content-Type: multipart/mixed; boundary=\"EXP\"\r
\r
--EXP\r
Content-Type: text/plain; charset=UTF-8\r
\r
Here is my calendar.\r
--EXP\r
Content-Type: text/calendar; charset=UTF-8; name=\"schedule.ics\"\r
Content-Disposition: attachment; filename=\"schedule.ics\"\r
\r
BEGIN:VCALENDAR\r
VERSION:2.0\r
PRODID:-//Me//Export//EN\r
BEGIN:VEVENT\r
UID:export-uid-1@example.com\r
SUMMARY:Standup\r
DTSTART:20260720T090000Z\r
DTEND:20260720T091500Z\r
END:VEVENT\r
END:VCALENDAR\r
--EXP--\r
";

// Plain multipart email with no calendar part.
const PLAIN_MULTIPART: &str = "\
From: Bob <bob@example.com>\r
To: me@example.com\r
Subject: Just a note\r
Date: Mon, 13 Jul 2026 09:00:00 +0000\r
Message-ID: <plain-1@example.com>\r
MIME-Version: 1.0\r
Content-Type: multipart/alternative; boundary=\"P\"\r
\r
--P\r
Content-Type: text/plain; charset=UTF-8\r
\r
Hello there.\r
--P\r
Content-Type: text/html; charset=UTF-8\r
\r
<p>Hello there.</p>\r
--P--\r
";

#[test]
fn outlook_inline_request_saves_sidecar_and_event() {
    let email = parse_rfc822_to_fetched_email(OUTLOOK_INLINE_REQUEST.as_bytes()).unwrap();
    assert!(email.calendar_ics.is_some());
    let ev = email.event.clone().expect("event parsed");
    assert_eq!(ev.method.as_deref(), Some("REQUEST"));
    assert_eq!(ev.uid.as_deref(), Some("outlook-uid-1@tum.de"));
    assert_eq!(ev.sequence, 2);
    assert_eq!(ev.start.as_deref(), Some("2026-07-20T14:00:00+02:00"));
    assert_eq!(ev.end.as_deref(), Some("2026-07-20T15:00:00+02:00"));
    assert_eq!(ev.location.as_deref(), Some("Room 4.12"));
    assert_eq!(ev.organizer.as_deref(), Some("chair@tum.de"));
    assert_eq!(ev.rsvp, "needs-action");
    assert_eq!(ev.attendees.len(), 1);

    // The invite lands as the sidecar-named attachment blob, and as the only
    // attachment: the inline calendar part is never stored twice.
    let ingested = ingest(&email, "inbox");
    assert_eq!(
        ingested.attachment_names(),
        vec![CALENDAR_SIDECAR_NAME.to_string()]
    );
    let ics = String::from_utf8(
        ingested
            .attachment_bytes(CALENDAR_SIDECAR_NAME)
            .expect("sidecar blob"),
    )
    .unwrap();
    assert!(ics.contains("UID:outlook-uid-1@tum.de"));
    assert!(ics.contains("METHOD:REQUEST"));
}

#[test]
fn google_ics_attachment_request_saves_sidecar_and_event() {
    let email = parse_rfc822_to_fetched_email(GOOGLE_ICS_ATTACHMENT_REQUEST.as_bytes()).unwrap();
    let ev = email.event.clone().expect("event parsed");
    assert_eq!(ev.uid.as_deref(), Some("google-uid-1@google.com"));
    assert_eq!(ev.start.as_deref(), Some("2026-07-20T12:00:00Z"));
    assert_eq!(ev.recurrence, "Weekly on Monday");

    // The `.ics` attachment becomes the sidecar blob, never a second copy.
    let ingested = ingest(&email, "inbox");
    assert_eq!(
        ingested.attachment_names(),
        vec![CALENDAR_SIDECAR_NAME.to_string()]
    );
}

#[test]
fn malformed_calendar_part_stays_a_regular_attachment() {
    // This fixture's payload does not parse as a VCALENDAR and carries no
    // `METHOD` property (the `method=REQUEST` lives only in the MIME
    // Content-Type header, not the body). Under the corrected classification an
    // iMIP invite is the first calendar part whose PAYLOAD is a VCALENDAR with a
    // METHOD, so this part is NOT an invite: no sidecar, no event block. Its
    // bytes are preserved as a regular calendar attachment instead (previously
    // it was lifted to the sidecar -- expectation intentionally changed; the new
    // behavior loses no bytes and is more correct).

    let email = parse_rfc822_to_fetched_email(MALFORMED_INVITE.as_bytes()).unwrap();
    assert!(email.calendar_ics.is_none(), "not lifted to a sidecar");
    assert!(email.event.is_none());
    assert_eq!(
        email.attachments.len(),
        1,
        "preserved as a regular attachment"
    );
    // Inline calendar part had no filename, so a `.ics` name is synthesized.
    assert!(
        email.attachments[0].filename.ends_with(".ics"),
        "synthesized .ics filename, got {}",
        email.attachments[0].filename
    );
    assert_eq!(
        email.attachments[0].content, b"this is not a valid vcalendar payload at all\r\n",
        "original bytes preserved intact"
    );

    let ingested = ingest(&email, "inbox");
    assert!(
        ingested.attachment_bytes(CALENDAR_SIDECAR_NAME).is_none(),
        "no sidecar blob for a non-invite calendar part"
    );
    assert_eq!(ingested.attachment_names().len(), 1);
}

#[test]
fn invite_plus_shared_ics_lifts_invite_and_keeps_document() {
    let email = parse_rfc822_to_fetched_email(INVITE_PLUS_SHARED_ICS.as_bytes()).unwrap();
    // The invite is lifted to the sidecar and parsed into an event block.
    let ev = email.event.clone().expect("invite parsed");
    assert_eq!(ev.uid.as_deref(), Some("invite-plus-uid@google.com"));
    assert!(email.calendar_ics.is_some());
    // The second, non-invite `.ics` document survives as a regular attachment
    // with its original filename and full bytes -- previously it was lost.
    assert_eq!(email.attachments.len(), 1, "shared .ics preserved");
    assert_eq!(email.attachments[0].filename, "my-calendar.ics");
    assert!(
        email.attachments[0].content.starts_with(b"BEGIN:VCALENDAR"),
        "document bytes preserved"
    );
    assert!(
        email.attachments[0]
            .content
            .windows(b"my-export-1@example.com".len())
            .any(|w| w == b"my-export-1@example.com"),
        "the shared export, not the invite, is the attachment"
    );

    let ingested = ingest(&email, "inbox");
    assert_eq!(
        ingested.attachment_names(),
        vec![
            CALENDAR_SIDECAR_NAME.to_string(),
            "my-calendar.ics".to_string()
        ],
        "invite first as the sidecar, then the shared document"
    );
    assert!(ingested
        .attachment_bytes("my-calendar.ics")
        .unwrap()
        .starts_with(b"BEGIN:VCALENDAR"));
}

#[test]
fn non_imip_ics_export_is_a_plain_attachment() {
    let email = parse_rfc822_to_fetched_email(SHARED_ICS_EXPORT_ONLY.as_bytes()).unwrap();
    // No METHOD -> not an invite: no sidecar, no event block.
    assert!(
        email.calendar_ics.is_none(),
        "no sidecar for a plain export"
    );
    assert!(email.event.is_none(), "no event block for a plain export");
    // Kept as a regular attachment with its original filename.
    assert_eq!(email.attachments.len(), 1);
    assert_eq!(email.attachments[0].filename, "schedule.ics");

    let ingested = ingest(&email, "inbox");
    assert_eq!(
        ingested.attachment_names(),
        vec!["schedule.ics".to_string()]
    );
    assert!(ingested.attachment_bytes(CALENDAR_SIDECAR_NAME).is_none());
}

#[test]
fn plain_multipart_email_is_unchanged() {
    let email = parse_rfc822_to_fetched_email(PLAIN_MULTIPART.as_bytes()).unwrap();
    assert!(email.calendar_ics.is_none());
    assert!(email.event.is_none());
    assert!(!email.has_attachments);

    let ingested = ingest(&email, "inbox");
    assert!(
        ingested.attachment_names().is_empty(),
        "a plain email references no attachment blobs"
    );
    let has_attachments: i64 = ingested
        .store
        .conn()
        .query_row(
            "SELECT has_attachments FROM messages WHERE id = ?1",
            [ingested.row],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(has_attachments, 0);
}

// ---------------------------------------------------------------------------
// Send-side round-trip (ticket #0028)
//
// Build a METHOD:REQUEST invite the way `mp send --invite` does, assemble the
// exact iMIP MIME tree via the shared `build_invite_mime_body`, then feed the
// formatted bytes back through the #0027 receive path. This proves the sent
// invite is picked up as an event (frontmatter + sidecar) -- the anchor #0030
// reconciliation relies on.
// ---------------------------------------------------------------------------

#[test]
fn sent_invite_roundtrips_through_receive_parser() {
    use chrono::{TimeZone, Utc};
    use lettre::message::Message;
    use mailypoppins::invite::{build_invite_ics, generate_uid, EventSpan, InviteSpec};
    use mailypoppins::send::build_invite_mime_body;

    let organizer = "chair@tum.de";
    let uid = generate_uid(organizer);
    let spec = InviteSpec {
        uid: uid.clone(),
        sequence: 0,
        organizer: organizer.to_string(),
        attendees: vec!["a@example.com".to_string(), "b@example.com".to_string()],
        // Text that exercises RFC 5545 escaping + folding across the wire.
        summary: "LOC Day planning, part 2; final".to_string(),
        span: EventSpan::Timed {
            start: Utc.with_ymd_and_hms(2026, 7, 20, 12, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 7, 20, 13, 0, 0).unwrap(),
        },
        location: Some("Room 4.12; TUM".to_string()),
        description: Some("Agenda:\n- item one\n- item two".to_string()),
    };
    let ics = build_invite_ics(&spec).unwrap();

    // Assemble the iMIP MIME message (same builder as the live send path).
    let body = build_invite_mime_body(
        "You are invited.",
        "<p>You are invited.</p>".to_string(),
        &ics,
    );
    let message: Message = Message::builder()
        .from("Chair <chair@tum.de>".parse().unwrap())
        .to("a@example.com".parse().unwrap())
        .cc("b@example.com".parse().unwrap())
        .subject(&spec.summary)
        .message_id(Some("<sent-invite-1@tum.de>".to_string()))
        .multipart(body)
        .unwrap();
    let raw = message.formatted();

    // Header tree sanity: the inline calendar part with method=REQUEST is the
    // contract; the application/ics attachment is the optional hardening.
    let raw_str = String::from_utf8_lossy(&raw);
    assert!(raw_str.contains("multipart/mixed"), "top-level mixed");
    assert!(
        raw_str.contains("multipart/alternative"),
        "alternative present"
    );
    assert!(
        raw_str.contains("text/calendar") && raw_str.to_lowercase().contains("method=request"),
        "inline text/calendar; method=REQUEST is the contract"
    );
    assert!(
        raw_str.contains("application/ics"),
        "optional application/ics hardening part present"
    );

    // Round-trip through the #0027 receive path.
    let email = parse_rfc822_to_fetched_email(&raw).unwrap();
    let ev = email
        .event
        .clone()
        .expect("sent invite parsed back as an event");
    assert_eq!(ev.method.as_deref(), Some("REQUEST"));
    assert_eq!(ev.uid.as_deref(), Some(uid.as_str()));
    assert_eq!(ev.sequence, 0);
    assert_eq!(
        ev.summary.as_deref(),
        Some("LOC Day planning, part 2; final")
    );
    assert_eq!(ev.location.as_deref(), Some("Room 4.12; TUM"));
    assert_eq!(ev.organizer.as_deref(), Some("chair@tum.de"));
    assert_eq!(ev.start.as_deref(), Some("2026-07-20T12:00:00Z"));
    assert_eq!(ev.end.as_deref(), Some("2026-07-20T13:00:00Z"));
    assert_eq!(ev.attendees.len(), 2);
    assert_eq!(ev.attendees[0].address, "a@example.com");
    assert_eq!(ev.attendees[1].address, "b@example.com");
    assert!(email.calendar_ics.is_some(), "sidecar bytes carried");

    // Ingest into the store and confirm the sidecar bytes are the ones kept.
    let ingested = ingest(&email, "sent");
    let ics = String::from_utf8(
        ingested
            .attachment_bytes(CALENDAR_SIDECAR_NAME)
            .expect("sidecar blob"),
    )
    .unwrap();
    assert!(ics.contains(&format!("UID:{uid}")));
    assert!(ics.contains("METHOD:REQUEST"));
}

// ---------------------------------------------------------------------------
// Reconciliation over the store (tickets #0030, #0038 scope item 6)
//
// The full-rescan test that lived here drove the `.md` writer to lay a sent
// invite and an incoming REPLY out on disk. That writer is gone (#0037 unit
// 4a) and reconciliation now derives attendee statuses from the `invite.ics`
// blobs of the account's rows, so the end-to-end coverage runs the same MIME
// fixtures through ingest and then through `reconcile::load_invites` -- the
// entry point the Calendar view's agenda is built on.
// ---------------------------------------------------------------------------

/// One store shared by several ingested messages, so a whole account can be
/// assembled and then reconciled (the single-message `ingest` above opens a
/// fresh store per call).
struct Mailstore {
    _tmp: tempfile::TempDir,
    store: mailypoppins::store::Store,
    blobs: mailypoppins::store::BlobStore,
}

impl Mailstore {
    fn new() -> Self {
        let tmp = tempdir().unwrap();
        let store = mailypoppins::store::Store::open(tmp.path().join("store.sqlite3")).unwrap();
        let blobs = mailypoppins::store::BlobStore::new(tmp.path().join("blobs"));
        Mailstore {
            _tmp: tmp,
            store,
            blobs,
        }
    }

    /// Ingest raw RFC822 bytes through the real parse + ingest path.
    fn ingest_raw(&self, raw: &[u8], mailbox: &str, uid: i64) -> i64 {
        let email = parse_rfc822_to_fetched_email(raw).unwrap();
        mailypoppins::ingest::ingest_message(
            &self.store,
            &self.blobs,
            &mailypoppins::ingest::IngestInput {
                account: "acct",
                mailbox,
                uid,
                email: &email,
                raw: None,
            },
        )
        .unwrap()
        .row_id
    }

    fn invites(&self) -> Vec<mailypoppins::reconcile::InviteMessage> {
        mailypoppins::reconcile::load_invites(&self.store, &self.blobs, "acct")
    }
}

/// The raw sent copy of one outbound invitation, built the way `send.invite`
/// builds it: the shared ICS builder and the shared iMIP MIME tree.
fn sent_invite_raw(spec: &mailypoppins::invite::InviteSpec, message_id: &str) -> Vec<u8> {
    let ics = mailypoppins::invite::build_invite_ics(spec).unwrap();
    let body = mailypoppins::send::build_invite_mime_body(
        "You are invited.",
        "<p>You are invited.</p>".to_string(),
        &ics,
    );
    lettre::message::Message::builder()
        .from("Me <me@example.com>".parse().unwrap())
        .to("me@gmail.example".parse().unwrap())
        .subject(&spec.summary)
        .message_id(Some(message_id.to_string()))
        .multipart(body)
        .unwrap()
        .formatted()
}

/// The kindergarten case: a closure first sent as a 00:00-00:00 timed event,
/// then re-sent as an all-day update with the same UID and SEQUENCE 1. Both
/// sent copies are in the store; the agenda shows one row, the update's, as a
/// bare date, and `mp calendar rebuild`'s fold reads both without complaint.
#[test]
fn an_all_day_update_replaces_the_timed_original_on_the_agenda() {
    use chrono::{NaiveDate, TimeZone, Utc};
    use mailypoppins::invite::{EventSpan, InviteSpec};

    let uid = "kita-closure-1@example.com";
    let original = InviteSpec {
        uid: uid.to_string(),
        sequence: 0,
        organizer: "me@example.com".to_string(),
        attendees: vec!["me@gmail.example".to_string()],
        summary: "Kita closed".to_string(),
        span: EventSpan::Timed {
            start: Utc.with_ymd_and_hms(2026, 12, 23, 23, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2027, 1, 8, 23, 0, 0).unwrap(),
        },
        location: None,
        description: None,
    };
    let update = InviteSpec {
        sequence: 1,
        span: EventSpan::AllDay {
            first: NaiveDate::from_ymd_opt(2026, 12, 24).unwrap(),
            last: NaiveDate::from_ymd_opt(2027, 1, 8).unwrap(),
        },
        ..original.clone()
    };

    let ms = Mailstore::new();
    let first = ms.ingest_raw(
        &sent_invite_raw(&original, "<kita-0@example.com>"),
        "sent",
        1,
    );
    let updated = ms.ingest_raw(&sent_invite_raw(&update, "<kita-1@example.com>"), "sent", 2);

    // The sent copy's sidecar keeps the VALUE=DATE form.
    let ics = mailypoppins::store::read::load_invite_ics(&ms.store, &ms.blobs, updated)
        .expect("the invite.ics blob");
    let ics = String::from_utf8(ics).unwrap();
    assert!(ics.contains("DTSTART;VALUE=DATE:20261224"), "ics=\n{ics}");
    assert!(ics.contains("DTEND;VALUE=DATE:20270109"), "ics=\n{ics}");
    assert!(ics.contains("SEQUENCE:1"), "ics=\n{ics}");

    let agenda = mailypoppins::agenda::load_events_for_account(
        &ms.store,
        &ms.blobs,
        "acct",
        "me@example.com",
    );
    assert_eq!(agenda.len(), 1, "one event, not two: {agenda:#?}");
    let row = &agenda[0];
    assert_eq!(row.row_id, updated, "the update wins");
    assert_eq!(row.event.sequence, 1);
    assert_eq!(row.start_display, "2026-12-24");
    assert_eq!(row.event.start.as_deref(), Some("2026-12-24T00:00:00"));
    assert_eq!(row.event.end.as_deref(), Some("2027-01-09T00:00:00"));
    assert!(row.is_organizer);
    assert!(!row.event.superseded && !row.cancelled);

    // Both versions reach the fold, each at its own sequence. Distinct rows,
    // because a sent copy built by `build_invite_mime_body` currently lands
    // with two `invite.ics` blobs (the inline part and the application/ics
    // hardening part), so `list_invites` yields each row twice.
    let mut versions: Vec<(i64, u32)> = ms
        .invites()
        .iter()
        .map(|i| (i.row_id, i.parsed.sequence))
        .collect();
    versions.dedup();
    assert_eq!(versions, vec![(first, 0), (updated, 1)]);
    let report = mailypoppins::reconcile::reconcile_account(&ms.store, &ms.blobs, "acct");
    assert_eq!(report.cancelled, 0);
}

/// Only the fixtures classified as iMIP invites reach the calendar path. The
/// shared `.ics` export, the malformed calendar part and the plain email are
/// ordinary messages: they carry no `invite.ics` blob, so the invite listing
/// never sees them and no agenda row can come from them.
#[test]
fn only_imip_classified_messages_reach_the_calendar_path() {
    let ms = Mailstore::new();
    ms.ingest_raw(OUTLOOK_INLINE_REQUEST.as_bytes(), "inbox", 1);
    ms.ingest_raw(GOOGLE_ICS_ATTACHMENT_REQUEST.as_bytes(), "inbox", 2);
    ms.ingest_raw(SHARED_ICS_EXPORT_ONLY.as_bytes(), "inbox", 3);
    ms.ingest_raw(MALFORMED_INVITE.as_bytes(), "inbox", 4);
    ms.ingest_raw(PLAIN_MULTIPART.as_bytes(), "inbox", 5);

    let invites = ms.invites();
    let uids: Vec<Option<&str>> = invites.iter().map(|i| i.uid()).collect();
    assert_eq!(
        uids,
        vec![
            Some("outlook-uid-1@tum.de"),
            Some("google-uid-1@google.com")
        ],
        "only the two iMIP REQUESTs are invites"
    );
    assert!(invites.iter().all(|i| i.method() == "REQUEST"));
    // The zoned Outlook DTSTART survives the round trip through the blob.
    assert_eq!(
        invites[0].parsed.start.as_deref(),
        Some("2026-07-20T14:00:00+02:00")
    );
    assert_eq!(invites[0].parsed.sequence, 2);
}

/// The attendee's round trip, end to end over the store: an Outlook REQUEST is
/// ingested, the RSVP reply is built exactly as `prepare_rsvp` builds it, and its
/// sent copy is ingested the way the outbox does during the send. Our own
/// `PARTSTAT` is then derived from that sent REPLY, with nothing written to the
/// invitation, so a second pass reports the same numbers.
#[test]
fn an_rsvp_reply_reconciles_against_the_stored_invite() {
    use mailypoppins::invite::Rsvp;

    let ms = Mailstore::new();
    let request = ms.ingest_raw(OUTLOOK_INLINE_REQUEST.as_bytes(), "inbox", 1);

    // Read the invitation back out of the store, exactly as the TUI does.
    let ics = mailypoppins::store::read::load_invite_ics(&ms.store, &ms.blobs, request)
        .expect("the invite.ics blob");
    let ctx = mailypoppins::invite::reply_context_from_ics(&ics).unwrap();
    let reply_ics =
        mailypoppins::invite::build_reply_ics(&ctx, "me@example.com", None, Rsvp::Declined)
            .unwrap();
    let built = mailypoppins::send::build_reply_message(
        "me@example.com",
        &ctx.organizer,
        "Declined: LOC Day planning",
        "Declined: LOC Day planning",
        &reply_ics,
        "",
    )
    .unwrap();
    // What `outbox::ingest_sent_copy` files during the send itself.
    ms.ingest_raw(&built.raw, "sent", 1);

    let invites = ms.invites();
    assert_eq!(invites.len(), 2, "the REQUEST and our sent REPLY");
    let replies = mailypoppins::reconcile::fold_replies(&invites);
    let by_addr = replies.get("outlook-uid-1@tum.de");

    let request = invites
        .iter()
        .find(|i| i.method() == "REQUEST")
        .expect("the REQUEST");
    let mut event = mailypoppins::calendar::event_frontmatter(&request.parsed);
    mailypoppins::reconcile::apply_replies(&mut event, request.parsed.sequence, by_addr);
    assert_eq!(event.attendees[0].address, "me@example.com");
    assert_eq!(event.attendees[0].status, "declined");
    assert_eq!(
        mailypoppins::reconcile::own_rsvp(&event, "me@example.com", by_addr),
        "declined"
    );

    // The report is a read: two passes, same numbers, nothing written.
    let first = mailypoppins::reconcile::reconcile_account(&ms.store, &ms.blobs, "acct");
    assert_eq!(first.invites_seen, 1);
    assert_eq!(first.replies_seen, 1);
    assert_eq!(first.resolved, 1);
    assert_eq!(
        mailypoppins::reconcile::reconcile_account(&ms.store, &ms.blobs, "acct"),
        first
    );
}

// ---------------------------------------------------------------------------
// The organizer's update and cancellation, as an attendee receives them
// (PERSO-204)
//
// Each row builds the change on the organizer's side exactly as the daemon
// does (the sent copy read back, the next SEQUENCE over the store, the iMIP
// message), then ingests that message into the attendee's own store and reads
// the attendee's agenda: what mp sends is what its receive half accepts.
// ---------------------------------------------------------------------------

/// The organizer's account: `Me <me@example.com>`.
fn organizer_account() -> mailypoppins::config::AccountConfig {
    mailypoppins::config::AccountConfig {
        name: "acct".to_string(),
        default_from: "Me <me@example.com>".to_string(),
        ..Default::default()
    }
}

/// The raw message one organizer change sends, built as the daemon builds it.
fn itip_raw(message: &mailypoppins::invite::ItipMessage) -> Vec<u8> {
    mailypoppins::invite::itip_built(
        &organizer_account(),
        "me@example.com",
        &mailypoppins::config::EmailSettings::default(),
        None,
        message,
    )
    .unwrap()
    .raw
}

fn review() -> mailypoppins::invite::InviteSpec {
    use chrono::{TimeZone, Utc};
    mailypoppins::invite::InviteSpec {
        uid: "review-1@example.com".to_string(),
        sequence: 0,
        organizer: "me@example.com".to_string(),
        attendees: vec!["ada@example.com".to_string(), "bob@example.com".to_string()],
        summary: "Budget review".to_string(),
        span: mailypoppins::invite::EventSpan::Timed {
            start: Utc.with_ymd_and_hms(2026, 12, 14, 9, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 12, 14, 10, 0, 0).unwrap(),
        },
        location: Some("Room 4.12".to_string()),
        description: None,
    }
}

/// The attendee `address`'s agenda over `store`.
fn agenda_of(store: &Mailstore, address: &str) -> Vec<mp_protocol::calendar::AgendaEvent> {
    mailypoppins::agenda::load_events_for_account(&store.store, &store.blobs, "acct", address)
}

#[test]
fn an_update_the_organizer_sends_moves_the_attendees_event() {
    use chrono::{TimeZone, Utc};
    use mailypoppins::calendar_model::EventTime;
    use mailypoppins::calendar_write::{AttendeeInput, EventPatch};

    let organizer = Mailstore::new();
    let sent = organizer.ingest_raw(
        &sent_invite_raw(&review(), "<review-0@example.com>"),
        "sent",
        1,
    );
    let ada = Mailstore::new();
    ada.ingest_raw(
        &sent_invite_raw(&review(), "<review-0@example.com>"),
        "inbox",
        1,
    );
    let bob = Mailstore::new();
    bob.ingest_raw(
        &sent_invite_raw(&review(), "<review-0@example.com>"),
        "inbox",
        1,
    );

    let row = mailypoppins::invite::organized_row(
        &organizer.store,
        &organizer.blobs,
        &organizer_account(),
        sent,
    )
    .unwrap();
    assert_eq!(row.invite.uid, "review-1@example.com");
    assert_eq!(row.next_sequence, 1);

    // A new time and room, and Bob no longer invited.
    let current = EventTime::timed(
        Utc.with_ymd_and_hms(2026, 12, 14, 9, 0, 0).unwrap(),
        Utc.with_ymd_and_hms(2026, 12, 14, 10, 0, 0).unwrap(),
        "UTC",
    )
    .unwrap();
    let patch = EventPatch {
        time: Some(
            EventTime::timed(
                Utc.with_ymd_and_hms(2026, 12, 15, 14, 0, 0).unwrap(),
                Utc.with_ymd_and_hms(2026, 12, 15, 15, 0, 0).unwrap(),
                "UTC",
            )
            .unwrap(),
        ),
        location: Some(Some("Room 1.01".to_string())),
        attendees: Some(vec![AttendeeInput {
            email: "ada@example.com".to_string(),
            name: None,
            role: mailypoppins::calendar_model::AttendeeRole::Required,
        }]),
        ..Default::default()
    };
    let (spec, messages) =
        mailypoppins::invite::update_messages(&row.invite, &current, &patch, row.next_sequence)
            .unwrap();
    // The cancellation of the dropped attendee goes first, one version
    // below the update, so the organizer's own sent copy of it is a stale
    // CANCEL that leaves the update standing.
    assert_eq!(spec.sequence, 2);
    let [cancel, update] = messages.as_slice() else {
        panic!("a cancellation and an update: {messages:#?}")
    };
    assert_eq!(cancel.to.as_deref(), Some("bob@example.com"));
    assert!(cancel.ics.contains("METHOD:CANCEL") && cancel.ics.contains("SEQUENCE:1"));
    assert_eq!(update.to.as_deref(), Some("ada@example.com"));
    assert!(update.ics.contains("METHOD:REQUEST") && update.ics.contains("SEQUENCE:2"));

    let update_raw = itip_raw(update);
    let text = String::from_utf8_lossy(&update_raw).to_lowercase();
    assert!(text.contains("text/calendar; method=request"), "{text}");
    ada.ingest_raw(&update_raw, "inbox", 2);
    let agenda = agenda_of(&ada, "ada@example.com");
    assert_eq!(
        agenda.len(),
        1,
        "the update replaces the invitation: {agenda:#?}"
    );
    let event = &agenda[0].event;
    assert_eq!(event.sequence, 2);
    assert_eq!(event.start.as_deref(), Some("2026-12-15T14:00:00Z"));
    assert_eq!(event.location.as_deref(), Some("Room 1.01"));
    assert!(!event.superseded && !event.cancelled);

    let cancel_raw = itip_raw(cancel);
    let text = String::from_utf8_lossy(&cancel_raw).to_lowercase();
    assert!(text.contains("text/calendar; method=cancel"), "{text}");
    bob.ingest_raw(&cancel_raw, "inbox", 2);
    let agenda = agenda_of(&bob, "bob@example.com");
    assert_eq!(agenda.len(), 1);
    assert!(agenda[0].cancelled, "Bob's copy is cancelled: {agenda:#?}");

    // The organizer's own agenda, once both sent copies are filed: the
    // update, live.
    organizer.ingest_raw(&cancel_raw, "sent", 2);
    organizer.ingest_raw(&update_raw, "sent", 3);
    let own = agenda_of(&organizer, "me@example.com");
    assert_eq!(own.len(), 1);
    assert!(own[0].is_organizer && !own[0].cancelled, "{own:#?}");
    assert_eq!(own[0].event.sequence, 2);
    let next = mailypoppins::invite::organized_row(
        &organizer.store,
        &organizer.blobs,
        &organizer_account(),
        own[0].row_id,
    )
    .unwrap();
    assert_eq!(next.next_sequence, 3, "the next change goes above both");
}

#[test]
fn a_cancel_the_organizer_sends_tombstones_the_attendees_event() {
    let organizer = Mailstore::new();
    let sent = organizer.ingest_raw(
        &sent_invite_raw(&review(), "<review-0@example.com>"),
        "sent",
        1,
    );
    let ada = Mailstore::new();
    ada.ingest_raw(
        &sent_invite_raw(&review(), "<review-0@example.com>"),
        "inbox",
        1,
    );

    let row = mailypoppins::invite::organized_row(
        &organizer.store,
        &organizer.blobs,
        &organizer_account(),
        sent,
    )
    .unwrap();
    let cancel = mailypoppins::invite::cancel_message(
        &row.invite,
        row.next_sequence,
        None,
        Some("The numbers are not in yet."),
    )
    .unwrap();
    assert_eq!(cancel.subject, "Cancelled: Budget review");
    assert_eq!(
        cancel.to.as_deref(),
        Some("ada@example.com, bob@example.com")
    );
    assert!(
        cancel.body.contains("The numbers are not in yet."),
        "{}",
        cancel.body
    );

    let raw = itip_raw(&cancel);
    ada.ingest_raw(&raw, "inbox", 2);
    let agenda = agenda_of(&ada, "ada@example.com");
    assert_eq!(agenda.len(), 1);
    assert!(agenda[0].cancelled, "{agenda:#?}");

    // On the organizer's own agenda too, once the sent copy is filed.
    organizer.ingest_raw(&raw, "sent", 2);
    assert!(agenda_of(&organizer, "me@example.com")[0].cancelled);
}

/// A weekly series this account organised, sent from Outlook in a Windows
/// zone, as its sent copy holds it.
const OWN_SERIES: &str = "\
From: Me <me@example.com>\r
To: ada@example.com\r
Subject: Weekly sync\r
Date: Mon, 30 Nov 2026 09:00:00 +0000\r
Message-ID: <weekly-1@example.com>\r
MIME-Version: 1.0\r
Content-Type: text/calendar; method=REQUEST; charset=UTF-8\r
Content-Transfer-Encoding: 7bit\r
\r
BEGIN:VCALENDAR\r
METHOD:REQUEST\r
PRODID:Microsoft Exchange Server 2010\r
VERSION:2.0\r
BEGIN:VTIMEZONE\r
TZID:W. Europe Standard Time\r
BEGIN:STANDARD\r
DTSTART:16010101T030000\r
TZOFFSETFROM:+0200\r
TZOFFSETTO:+0100\r
RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=10\r
END:STANDARD\r
BEGIN:DAYLIGHT\r
DTSTART:16010101T020000\r
TZOFFSETFROM:+0100\r
TZOFFSETTO:+0200\r
RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=3\r
END:DAYLIGHT\r
END:VTIMEZONE\r
BEGIN:VEVENT\r
ORGANIZER;CN=Me:mailto:me@example.com\r
ATTENDEE;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:ada@example.com\r
RRULE:FREQ=WEEKLY;UNTIL=20270131T080000Z;INTERVAL=1;BYDAY=MO;WKST=MO\r
UID:weekly-1@example.com\r
SUMMARY:Weekly sync\r
DTSTART;TZID=W. Europe Standard Time:20261207T090000\r
DTEND;TZID=W. Europe Standard Time:20261207T093000\r
SEQUENCE:0\r
DTSTAMP:20261130T090000Z\r
END:VEVENT\r
END:VCALENDAR\r
";

#[test]
fn an_occurrence_the_organizer_cancels_leaves_the_attendees_series_live() {
    let organizer = Mailstore::new();
    let sent = organizer.ingest_raw(OWN_SERIES.as_bytes(), "sent", 1);
    let ada = Mailstore::new();
    ada.ingest_raw(OWN_SERIES.as_bytes(), "inbox", 1);

    let row = mailypoppins::invite::organized_row(
        &organizer.store,
        &organizer.blobs,
        &organizer_account(),
        sent,
    )
    .unwrap();
    let berlin: chrono_tz::Tz = "Europe/Berlin".parse().unwrap();
    let rid =
        mailypoppins::invite::occurrence_recurrence_id(&row.invite, "2026-12-21T09:00", berlin)
            .unwrap();
    let cancel =
        mailypoppins::invite::cancel_message(&row.invite, row.next_sequence, Some(&rid), None)
            .unwrap();
    ada.ingest_raw(&itip_raw(&cancel), "inbox", 2);

    let agenda = agenda_of(&ada, "ada@example.com");
    assert_eq!(agenda.len(), 1, "{agenda:#?}");
    assert!(!agenda[0].cancelled, "the series lives on");
    assert_eq!(
        agenda[0].event.cancelled_instances,
        ["2026-12-21T09:00:00+01:00"]
    );
}

#[test]
fn only_an_invitation_the_account_organised_is_changed() {
    let store = Mailstore::new();
    let received = store.ingest_raw(OUTLOOK_INLINE_REQUEST.as_bytes(), "inbox", 1);
    let err = mailypoppins::invite::organized_row(
        &store.store,
        &store.blobs,
        &organizer_account(),
        received,
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("organised by chair@tum.de"),
        "{err:#}"
    );
    let plain = store.ingest_raw(PLAIN_MULTIPART.as_bytes(), "sent", 2);
    let err = mailypoppins::invite::organized_row(
        &store.store,
        &store.blobs,
        &organizer_account(),
        plain,
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("carries no invitation"),
        "{err:#}"
    );
}
