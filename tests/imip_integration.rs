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
    use email::invite::{build_invite_ics, generate_uid, InviteSpec};
    use email::send::build_invite_mime_body;
    use lettre::message::Message;

    let organizer = "chair@tum.de";
    let uid = generate_uid(organizer);
    let spec = InviteSpec {
        uid: uid.clone(),
        organizer: organizer.to_string(),
        attendees: vec!["a@example.com".to_string(), "b@example.com".to_string()],
        // Text that exercises RFC 5545 escaping + folding across the wire.
        summary: "LOC Day planning, part 2; final".to_string(),
        start: Utc.with_ymd_and_hms(2026, 7, 20, 12, 0, 0).unwrap(),
        end: Utc.with_ymd_and_hms(2026, 7, 20, 13, 0, 0).unwrap(),
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
    assert!(raw_str.contains("multipart/alternative"), "alternative present");
    assert!(
        raw_str.contains("text/calendar")
            && raw_str.to_lowercase().contains("method=request"),
        "inline text/calendar; method=REQUEST is the contract"
    );
    assert!(
        raw_str.contains("application/ics"),
        "optional application/ics hardening part present"
    );

    // Round-trip through the #0027 receive path.
    let email = parse_rfc822_to_fetched_email(&raw).unwrap();
    let ev = email.event.clone().expect("sent invite parsed back as an event");
    assert_eq!(ev.method.as_deref(), Some("REQUEST"));
    assert_eq!(ev.uid.as_deref(), Some(uid.as_str()));
    assert_eq!(ev.sequence, 0);
    assert_eq!(ev.summary.as_deref(), Some("LOC Day planning, part 2; final"));
    assert_eq!(ev.location.as_deref(), Some("Room 4.12; TUM"));
    assert_eq!(ev.organizer.as_deref(), Some("chair@tum.de"));
    assert_eq!(ev.start.as_deref(), Some("2026-07-20T12:00:00Z"));
    assert_eq!(ev.end.as_deref(), Some("2026-07-20T13:00:00Z"));
    assert_eq!(ev.attendees.len(), 2);
    assert_eq!(ev.attendees[0].address, "a@example.com");
    assert_eq!(ev.attendees[1].address, "b@example.com");
    assert!(email.calendar_ics.is_some(), "sidecar bytes carried");

    // Save to a mailbox and confirm the sidecar + event block land on disk.
    let tmp = tempdir().unwrap();
    let sent = tmp.path().join("acct").join("sent");
    save_fetched_emails(&[email], &sent, "sent").unwrap();
    let (md, content) = read_only_md(&sent);
    let att_dir = md.with_file_name(format!(
        "{}_attachments",
        md.file_stem().unwrap().to_string_lossy()
    ));
    assert!(att_dir.join(CALENDAR_SIDECAR_NAME).exists(), "sidecar written");
    assert!(content.contains("event:"));
    assert!(content.contains(&format!("uid: {}", uid)));
    assert!(content.contains("method: REQUEST"));
}

/// Full-rescan reconciliation (#0030) through the real save path.
///
/// Builds a sent REQUEST invite and an incoming REPLY with the same UID via the
/// live iMIP builders, saves both to disk through `save_fetched_emails` (the
/// production writer that emits the sidecar + `event:` block), then runs
/// `reconcile_account` over the account root and asserts the sent invite's
/// attendee status flips. A second run must be byte-identical (idempotent).
#[test]
fn rescan_reconciles_reply_into_sent_invite_and_is_idempotent() {
    use chrono::{TimeZone, Utc};
    use email::invite::{
        build_invite_ics, build_reply_ics, generate_uid, reply_context_from_ics, InviteSpec, Rsvp,
    };
    use email::reconcile::reconcile_account;
    use email::send::build_invite_mime_body;
    use lettre::message::Message;

    let organizer = "chair@tum.de";
    let uid = generate_uid(organizer);
    let spec = InviteSpec {
        uid: uid.clone(),
        organizer: organizer.to_string(),
        attendees: vec!["a@example.com".to_string(), "b@example.com".to_string()],
        summary: "LOC Day planning".to_string(),
        start: Utc.with_ymd_and_hms(2026, 7, 20, 12, 0, 0).unwrap(),
        end: Utc.with_ymd_and_hms(2026, 7, 20, 13, 0, 0).unwrap(),
        location: Some("Room 4.12".to_string()),
        description: None,
    };
    let request_ics = build_invite_ics(&spec).unwrap();

    // --- Sent REQUEST invite (organizer's Sent copy) ---
    let req_body = build_invite_mime_body("You are invited.", String::new(), &request_ics);
    let req_msg: Message = Message::builder()
        .from("Chair <chair@tum.de>".parse().unwrap())
        .to("a@example.com".parse().unwrap())
        .cc("b@example.com".parse().unwrap())
        .subject(&spec.summary)
        .message_id(Some("<sent-invite-30@tum.de>".to_string()))
        .multipart(req_body)
        .unwrap();
    let req_raw = req_msg.formatted();
    let req_email = parse_rfc822_to_fetched_email(&req_raw).unwrap();

    // --- Incoming REPLY from attendee A (ACCEPTED) ---
    let ctx = reply_context_from_ics(request_ics.as_bytes()).unwrap();
    let reply_ics = build_reply_ics(&ctx, "a@example.com", Rsvp::Accepted).unwrap();
    let reply_body = build_invite_mime_body("A accepts.", String::new(), &reply_ics);
    let reply_msg: Message = Message::builder()
        .from("A <a@example.com>".parse().unwrap())
        .to("chair@tum.de".parse().unwrap())
        .subject("Accepted: LOC Day planning")
        .message_id(Some("<reply-a-30@example.com>".to_string()))
        .multipart(reply_body)
        .unwrap();
    let reply_raw = reply_msg.formatted();
    let reply_email = parse_rfc822_to_fetched_email(&reply_raw).unwrap();
    assert_eq!(
        reply_email.event.as_ref().and_then(|e| e.method.as_deref()),
        Some("REPLY"),
        "reply parsed as METHOD:REPLY"
    );

    // --- Save both through the production writer ---
    let tmp = tempdir().unwrap();
    let account_root = tmp.path().join("acct");
    let sent = account_root.join("sent");
    let inbox = account_root.join("inbox");
    save_fetched_emails(&[req_email], &sent, "sent").unwrap();
    save_fetched_emails(&[reply_email], &inbox, "inbox").unwrap();

    let (sent_md, before) = read_only_md(&sent);
    // Attendee A starts unresolved on the sent invite.
    let fm_before = parse_frontmatter(&before).event.unwrap();
    let a_before = fm_before
        .attendees
        .iter()
        .find(|x| x.address.eq_ignore_ascii_case("a@example.com"))
        .unwrap();
    assert_eq!(a_before.status, "needs-action");

    // --- Full-mailstore reconciliation ---
    let stats = reconcile_account(&account_root).unwrap();
    assert_eq!(stats.updated, 1, "exactly one attendee status rewritten");

    let after_first = fs::read(&sent_md).unwrap();
    let fm_after = parse_frontmatter(&String::from_utf8_lossy(&after_first))
        .event
        .unwrap();
    let a_after = fm_after
        .attendees
        .iter()
        .find(|x| x.address.eq_ignore_ascii_case("a@example.com"))
        .unwrap();
    assert_eq!(a_after.status, "accepted", "A's RSVP reconciled onto the invite");
    // B never replied, so stays needs-action.
    let b_after = fm_after
        .attendees
        .iter()
        .find(|x| x.address.eq_ignore_ascii_case("b@example.com"))
        .unwrap();
    assert_eq!(b_after.status, "needs-action");

    // --- Idempotency: second rescan changes nothing, bytes identical ---
    let stats2 = reconcile_account(&account_root).unwrap();
    assert_eq!(stats2.updated, 0);
    assert_eq!(stats2.unchanged, 1);
    let after_second = fs::read(&sent_md).unwrap();
    assert_eq!(after_first, after_second, "rescan must be byte-idempotent");
}
