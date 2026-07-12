//! End-to-end iMIP receive tests (ticket #0027): raw RFC822 MIME -> parse ->
//! save. Verifies that `text/calendar` parts are detected, the sidecar `.ics`
//! is written next to the email, the `event:` frontmatter block is populated,
//! and that a plain email without a calendar part is stored exactly as before.

use email::parse::{
    parse_rfc822_to_fetched_email, save_fetched_emails, CALENDAR_SIDECAR_NAME,
};
use email::types::InboxFrontmatter;
use gray_matter::{engine::YAML, Matter};
use std::fs;
use tempfile::tempdir;

fn parse_frontmatter(content: &str) -> InboxFrontmatter {
    let matter = Matter::<YAML>::new();
    let parsed = matter.parse(content);
    parsed.data.unwrap().deserialize().unwrap()
}

/// Read the single saved .md file in a mailbox directory.
fn read_only_md(dir: &std::path::Path) -> (std::path::PathBuf, String) {
    let md = fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "md"))
        .expect("expected one .md file");
    let content = fs::read_to_string(&md).unwrap();
    (md, content)
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
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("acct").join("inbox");

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

    save_fetched_emails(&[email], &inbox, "inbox").unwrap();
    let (md, content) = read_only_md(&inbox);

    // Sidecar written next to the email in its attachments dir.
    let att_dir = md.with_file_name(format!(
        "{}_attachments",
        md.file_stem().unwrap().to_string_lossy()
    ));
    let sidecar = att_dir.join(CALENDAR_SIDECAR_NAME);
    assert!(sidecar.exists(), "sidecar .ics should exist");
    let ics = fs::read_to_string(&sidecar).unwrap();
    assert!(ics.contains("UID:outlook-uid-1@tum.de"));

    // Frontmatter carries the event block.
    assert!(content.contains("event:"));
    assert!(content.contains("uid: outlook-uid-1@tum.de"));
    assert!(content.contains("method: REQUEST"));
    assert!(content.contains("start: 2026-07-20T14:00:00+02:00"));
    assert!(content.contains("rsvp: needs-action"));

    // The inline calendar part is NOT stored as a duplicate attachment, and the
    // frontmatter does not claim attachments.
    let fm = parse_frontmatter(&content);
    assert!(fm.attachments.is_none());
    assert!(!att_dir.join("invite-1.ics").exists());
}

#[test]
fn google_ics_attachment_request_saves_sidecar_and_event() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("acct").join("inbox");

    let email = parse_rfc822_to_fetched_email(GOOGLE_ICS_ATTACHMENT_REQUEST.as_bytes()).unwrap();
    let ev = email.event.clone().expect("event parsed");
    assert_eq!(ev.uid.as_deref(), Some("google-uid-1@google.com"));
    assert_eq!(ev.start.as_deref(), Some("2026-07-20T12:00:00Z"));
    assert_eq!(ev.recurrence, "Weekly on Monday");

    save_fetched_emails(&[email], &inbox, "inbox").unwrap();
    let (md, content) = read_only_md(&inbox);

    let att_dir = md.with_file_name(format!(
        "{}_attachments",
        md.file_stem().unwrap().to_string_lossy()
    ));
    assert!(att_dir.join(CALENDAR_SIDECAR_NAME).exists());
    assert!(content.contains("recurrence: Weekly on Monday"));

    // The `.ics` attachment is stored as the sidecar, never as a regular
    // attachment file, so there is no `invite.ics` duplicate outside the sidecar.
    let entries: Vec<String> = fs::read_dir(&att_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(entries, vec![CALENDAR_SIDECAR_NAME.to_string()]);
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
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("acct").join("inbox");

    let email = parse_rfc822_to_fetched_email(MALFORMED_INVITE.as_bytes()).unwrap();
    assert!(email.calendar_ics.is_none(), "not lifted to a sidecar");
    assert!(email.event.is_none());
    assert_eq!(email.attachments.len(), 1, "preserved as a regular attachment");
    // Inline calendar part had no filename, so a `.ics` name is synthesized.
    assert!(
        email.attachments[0].filename.ends_with(".ics"),
        "synthesized .ics filename, got {}",
        email.attachments[0].filename
    );
    assert_eq!(
        email.attachments[0].content,
        b"this is not a valid vcalendar payload at all\r\n",
        "original bytes preserved intact"
    );

    save_fetched_emails(&[email], &inbox, "inbox").unwrap();
    let (md, content) = read_only_md(&inbox);

    let att_dir = md.with_file_name(format!(
        "{}_attachments",
        md.file_stem().unwrap().to_string_lossy()
    ));
    assert!(
        !att_dir.join(CALENDAR_SIDECAR_NAME).exists(),
        "no sidecar for a non-invite calendar part"
    );
    assert!(!content.contains("event:"), "no event block for a non-invite");
}

#[test]
fn invite_plus_shared_ics_lifts_invite_and_keeps_document() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("acct").join("inbox");

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
        email.attachments[0].content.windows(b"my-export-1@example.com".len())
            .any(|w| w == b"my-export-1@example.com"),
        "the shared export, not the invite, is the attachment"
    );

    save_fetched_emails(&[email], &inbox, "inbox").unwrap();
    let (md, content) = read_only_md(&inbox);
    let att_dir = md.with_file_name(format!(
        "{}_attachments",
        md.file_stem().unwrap().to_string_lossy()
    ));
    assert!(att_dir.join(CALENDAR_SIDECAR_NAME).exists(), "sidecar written");
    assert!(att_dir.join("my-calendar.ics").exists(), "document written");
    assert!(content.contains("event:"));
}

#[test]
fn non_imip_ics_export_is_a_plain_attachment() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("acct").join("inbox");

    let email = parse_rfc822_to_fetched_email(SHARED_ICS_EXPORT_ONLY.as_bytes()).unwrap();
    // No METHOD -> not an invite: no sidecar, no event block.
    assert!(email.calendar_ics.is_none(), "no sidecar for a plain export");
    assert!(email.event.is_none(), "no event block for a plain export");
    // Kept as a regular attachment with its original filename.
    assert_eq!(email.attachments.len(), 1);
    assert_eq!(email.attachments[0].filename, "schedule.ics");

    save_fetched_emails(&[email], &inbox, "inbox").unwrap();
    let (md, content) = read_only_md(&inbox);
    let att_dir = md.with_file_name(format!(
        "{}_attachments",
        md.file_stem().unwrap().to_string_lossy()
    ));
    assert!(
        !att_dir.join(CALENDAR_SIDECAR_NAME).exists(),
        "no invite.ics sidecar"
    );
    assert!(att_dir.join("schedule.ics").exists(), "kept as attachment");
    assert!(!content.contains("event:"));
}

#[test]
fn plain_multipart_email_is_unchanged() {
    let tmp = tempdir().unwrap();
    let inbox = tmp.path().join("acct").join("inbox");

    let email = parse_rfc822_to_fetched_email(PLAIN_MULTIPART.as_bytes()).unwrap();
    assert!(email.calendar_ics.is_none());
    assert!(email.event.is_none());
    assert!(!email.has_attachments);

    save_fetched_emails(&[email], &inbox, "inbox").unwrap();
    let (md, content) = read_only_md(&inbox);

    assert!(!content.contains("event:"));
    // No attachments directory is created for a plain email.
    let att_dir = md.with_file_name(format!(
        "{}_attachments",
        md.file_stem().unwrap().to_string_lossy()
    ));
    assert!(!att_dir.exists());

    let fm = parse_frontmatter(&content);
    assert!(fm.event.is_none());
    assert!(fm.attachments.is_none());
}
