//! iCalendar (iMIP) send-side building (ticket #0028).
//!
//! Builds a `METHOD:REQUEST` `VCALENDAR`/`VEVENT` from CLI/library input and
//! renders it to the `.ics` bytes that go into the outgoing iMIP MIME message
//! (see `docs/plans/calendar-invites.md`, D1/D7, §2). This is the counterpart
//! to the receive-side parser in [`crate::calendar`]; both use the `icalendar`
//! crate so escaping (RFC 5545 TEXT) and folding are handled by one dependency.
//!
//! The produced `.ics` round-trips through the #0027 receive path: it carries a
//! `METHOD` property (so [`crate::calendar::is_imip_invite`] lifts it to a
//! sidecar) and a single `VEVENT` that [`crate::calendar::parse_ics`] can read
//! back into an `event:` frontmatter block.

use std::str::FromStr;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, NaiveDateTime, TimeZone, Utc};
use icalendar::{Calendar, CalendarComponent, Component, Event, EventLike, Property};

/// The inputs needed to build a `REQUEST` invite. Times are already resolved to
/// absolute instants (UTC); recipients are bare email addresses.
#[derive(Debug, Clone)]
pub struct InviteSpec {
    /// Stable event `UID` (generate once, persist in the sent copy).
    pub uid: String,
    /// `ORGANIZER` — must equal the sending account's primary address.
    pub organizer: String,
    /// `ATTENDEE`s (from `--to`/`--cc`), deduplicated, bare addresses.
    pub attendees: Vec<String>,
    /// `SUMMARY` (the invite subject).
    pub summary: String,
    /// `DTSTART` (absolute instant).
    pub start: DateTime<Utc>,
    /// `DTEND` (absolute instant); must be strictly after `start`.
    pub end: DateTime<Utc>,
    /// Optional `LOCATION`.
    pub location: Option<String>,
    /// Optional `DESCRIPTION`.
    pub description: Option<String>,
}

/// Generate a stable, collision-resistant event `UID` without pulling in a UUID
/// dependency.
///
/// Format: `<40-hex-sha256-prefix>@<sender-domain>`. The hash digests the
/// current time (nanosecond precision), 16 bytes of OS randomness, and the
/// organizer address, so two invites generated on the same host in the same
/// nanosecond still differ. The `@domain` suffix (taken from the organizer's
/// email) makes it a globally-scoped RFC 5545 UID and keeps it recognisable in
/// logs / sidecars. Falls back to `mailypoppins` when the organizer has no
/// `@domain` part.
pub fn generate_uid(organizer: &str) -> String {
    use rand::RngCore;
    use sha2::{Digest, Sha256};

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let mut rand_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut rand_bytes);

    let mut hasher = Sha256::new();
    hasher.update(now.as_nanos().to_le_bytes());
    hasher.update(rand_bytes);
    hasher.update(organizer.as_bytes());
    let digest = hasher.finalize();

    // 20 bytes -> 40 hex chars: ample collision resistance for event UIDs.
    let hex: String = digest
        .iter()
        .take(20)
        .map(|b| format!("{:02x}", b))
        .collect();

    let domain = organizer
        .rsplit('@')
        .next()
        .filter(|d| !d.is_empty() && *d != organizer)
        .unwrap_or("mailypoppins");

    format!("{}@{}", hex, domain)
}

/// Parse a user-supplied datetime into an absolute UTC instant.
///
/// Accepted forms (documented in `mp send --help`):
/// - RFC 3339 with an explicit offset: `2026-07-20T14:00:00+02:00`, `...Z`.
/// - Local wall-clock (no offset), interpreted in the machine's local zone:
///   `2026-07-20T14:00:00`, `2026-07-20T14:00`, `2026-07-20 14:00`.
///
/// Local wall-clock inputs are resolved against the system timezone; ambiguous
/// or non-existent local times (DST folds/gaps) resolve to the earliest valid
/// instant, and an error is returned only when no valid instant exists.
pub fn parse_datetime(input: &str) -> Result<DateTime<Utc>> {
    let s = input.trim();

    // 1. RFC 3339 (explicit offset or Z).
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }

    // 2. Local wall-clock, several separators / precisions.
    let naive = parse_naive_local(s)
        .ok_or_else(|| anyhow!("Unrecognized datetime '{}'. Use RFC3339 (2026-07-20T14:00:00+02:00) or local time (2026-07-20T14:00).", input))?;

    match chrono::Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => Ok(dt.with_timezone(&Utc)),
        chrono::LocalResult::Ambiguous(earliest, _) => Ok(earliest.with_timezone(&Utc)),
        chrono::LocalResult::None => Err(anyhow!(
            "Local time '{}' does not exist in the local timezone (DST gap)",
            input
        )),
    }
}

fn parse_naive_local(s: &str) -> Option<NaiveDateTime> {
    const FORMATS: &[&str] = &[
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
    ];
    FORMATS
        .iter()
        .find_map(|fmt| NaiveDateTime::parse_from_str(s, fmt).ok())
}

/// Parse an ISO 8601 duration (e.g. `PT1H`, `PT30M`, `P1DT2H`) or a short
/// human form (`1h`, `90m`, `1h30m`) into a [`chrono::Duration`].
///
/// Only positive, day/hour/minute/second granularity is supported (no months
/// or years — a calendar invite duration must be an exact span).
pub fn parse_duration(input: &str) -> Result<Duration> {
    let s = input.trim();
    if s.is_empty() {
        return Err(anyhow!("Empty duration"));
    }

    let secs = if let Some(rest) = s.strip_prefix('P').or_else(|| s.strip_prefix('p')) {
        parse_iso8601_duration(rest)
    } else {
        parse_short_duration(s)
    }
    .ok_or_else(|| anyhow!("Unrecognized duration '{}'. Use ISO8601 (PT1H30M) or short form (1h30m).", input))?;

    if secs <= 0 {
        return Err(anyhow!("Duration must be positive: {}", input));
    }
    Ok(Duration::seconds(secs))
}

/// Parse the part after `P` in an ISO 8601 duration: `[nD][T[nH][nM][nS]]`.
fn parse_iso8601_duration(rest: &str) -> Option<i64> {
    let rest = rest.to_ascii_uppercase();
    let (date_part, time_part) = match rest.split_once('T') {
        Some((d, t)) => (d, t),
        None => (rest.as_str(), ""),
    };

    let mut total: i64 = 0;
    let mut any = false;

    // Date part supports only days (weeks/months/years are not exact spans).
    if !date_part.is_empty() {
        let mut num = String::new();
        for c in date_part.chars() {
            if c.is_ascii_digit() {
                num.push(c);
            } else if c == 'D' {
                total += num.parse::<i64>().ok()? * 86_400;
                num.clear();
                any = true;
            } else {
                return None;
            }
        }
        if !num.is_empty() {
            return None; // trailing digits with no unit
        }
    }

    // Time part: H, M, S.
    let mut num = String::new();
    for c in time_part.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            let n: i64 = num.parse().ok()?;
            num.clear();
            match c {
                'H' => total += n * 3_600,
                'M' => total += n * 60,
                'S' => total += n,
                _ => return None,
            }
            any = true;
        }
    }
    if !num.is_empty() {
        return None;
    }

    if any {
        Some(total)
    } else {
        None
    }
}

/// Parse a short human duration: `1h`, `30m`, `1h30m`, `2d`, `45s`.
fn parse_short_duration(s: &str) -> Option<i64> {
    let s = s.to_ascii_lowercase();
    let mut total: i64 = 0;
    let mut num = String::new();
    let mut any = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            let n: i64 = num.parse().ok()?;
            num.clear();
            match c {
                'd' => total += n * 86_400,
                'h' => total += n * 3_600,
                'm' => total += n * 60,
                's' => total += n,
                _ => return None,
            }
            any = true;
        }
    }
    if !num.is_empty() {
        return None; // trailing bare number with no unit
    }
    if any {
        Some(total)
    } else {
        None
    }
}

/// Build and validate an [`InviteSpec`], then render it to `.ics` bytes.
///
/// Validation: `end` strictly after `start`, at least one attendee, non-empty
/// `SUMMARY` and `ORGANIZER`. `ORGANIZER == account primary address` is enforced
/// by the caller (it owns the account identity); see `send_invite`.
pub fn build_invite_ics(spec: &InviteSpec) -> Result<String> {
    if spec.summary.trim().is_empty() {
        return Err(anyhow!("Invite summary is empty"));
    }
    if spec.organizer.trim().is_empty() {
        return Err(anyhow!("Invite organizer is empty"));
    }
    if spec.attendees.is_empty() {
        return Err(anyhow!("Invite has no attendees (need at least one --to/--cc)"));
    }
    if spec.end <= spec.start {
        return Err(anyhow!("Invite end must be after start"));
    }

    let mut event = Event::new();
    event
        .uid(&spec.uid)
        .sequence(0)
        .timestamp(Utc::now()) // DTSTAMP
        .summary(&spec.summary)
        .starts(spec.start)
        .ends(spec.end);

    if let Some(loc) = spec.location.as_deref().filter(|s| !s.trim().is_empty()) {
        event.location(loc);
    }
    if let Some(desc) = spec.description.as_deref().filter(|s| !s.trim().is_empty()) {
        event.description(desc);
    }

    // ORGANIZER (CAL-ADDRESS). The icalendar builder has no organizer helper,
    // so append the property directly; `mailto:` prefix per RFC 5545 §3.8.4.3.
    event.append_property(Property::new(
        "ORGANIZER",
        format!("mailto:{}", spec.organizer),
    ));

    // One ATTENDEE per recipient, RSVP=TRUE so clients surface Accept/Decline.
    for addr in &spec.attendees {
        let mut prop = Property::new("ATTENDEE", format!("mailto:{}", addr));
        prop.add_parameter("ROLE", "REQ-PARTICIPANT")
            .add_parameter("PARTSTAT", "NEEDS-ACTION")
            .add_parameter("RSVP", "TRUE");
        event.append_multi_property(prop);
    }

    let mut calendar = Calendar::new();
    calendar.push(event.done());
    // METHOD:REQUEST at the VCALENDAR level (the iMIP method). Setting it here
    // also drives the `method=REQUEST` MIME parameter in `send.rs`. The
    // `icalendar` crate already emits VERSION/PRODID/CALSCALE, so we only add
    // METHOD (adding a second PRODID would be invalid RFC 5545).
    calendar.append_property(Property::new("METHOD", "REQUEST"));

    Ok(calendar.to_string())
}

/// The three RSVP participation statuses an attendee can reply with (D3/D6).
/// Whole-series only in v1 (no per-occurrence responses).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rsvp {
    Accepted,
    Tentative,
    Declined,
}

impl Rsvp {
    /// RFC 5545 `PARTSTAT` value for the REPLY `ATTENDEE`.
    pub fn partstat(self) -> &'static str {
        match self {
            Rsvp::Accepted => "ACCEPTED",
            Rsvp::Tentative => "TENTATIVE",
            Rsvp::Declined => "DECLINED",
        }
    }

    /// Lowercase status stored in `event.rsvp` frontmatter.
    pub fn frontmatter_status(self) -> &'static str {
        match self {
            Rsvp::Accepted => "accepted",
            Rsvp::Tentative => "tentative",
            Rsvp::Declined => "declined",
        }
    }

    /// Outlook-style subject verb prefix (`Accepted:` / `Tentative:` /
    /// `Declined:`).
    pub fn subject_verb(self) -> &'static str {
        match self {
            Rsvp::Accepted => "Accepted",
            Rsvp::Tentative => "Tentative",
            Rsvp::Declined => "Declined",
        }
    }
}

/// What we extracted from a received invite's sidecar `.ics` in order to build
/// a REPLY. The sidecar is the source of truth for `UID`/`SEQUENCE`
/// (`docs/plans/calendar-invites.md`, D2) — never the frontmatter cache.
#[derive(Debug, Clone)]
pub struct ReplyContext {
    pub uid: String,
    pub sequence: u32,
    pub summary: Option<String>,
    /// `ORGANIZER` address (bare, no `mailto:`) — the REPLY recipient.
    pub organizer: String,
}

/// Strip a leading `mailto:` (case-insensitive) from a CAL-ADDRESS.
fn strip_mailto(addr: &str) -> &str {
    let trimmed = addr.trim();
    if trimmed.len() >= 7 && trimmed[..7].eq_ignore_ascii_case("mailto:") {
        trimmed[7..].trim()
    } else {
        trimmed
    }
}

/// Read a received invite's sidecar `.ics` bytes and pull out the fields needed
/// to build (and route) a REPLY: `UID`, `SEQUENCE`, `SUMMARY`, `ORGANIZER`.
///
/// This deliberately reads the raw `.ics` rather than the `event:` frontmatter
/// so the reply echoes the exact `UID`/`SEQUENCE` the organizer sent (the
/// frontmatter is a lossy render cache). Errors when the payload is not a
/// received `METHOD:REQUEST` invite, or is missing a `UID`/`ORGANIZER`.
pub fn reply_context_from_ics(bytes: &[u8]) -> Result<ReplyContext> {
    let text = std::str::from_utf8(bytes).context("Sidecar .ics is not valid UTF-8")?;
    let calendar =
        Calendar::from_str(text).map_err(|e| anyhow!("Failed to parse sidecar .ics: {}", e))?;

    let method = calendar
        .property_value("METHOD")
        .map(|m| m.trim().to_uppercase());
    match method.as_deref() {
        Some("REQUEST") => {}
        Some("REPLY") => {
            return Err(anyhow!(
                "This is a REPLY (an incoming RSVP), not an invitation you can respond to"
            ))
        }
        Some(other) => {
            return Err(anyhow!(
                "Cannot RSVP to a METHOD:{} calendar item (only received REQUEST invites)",
                other
            ))
        }
        None => return Err(anyhow!("Sidecar .ics has no METHOD (not an iMIP invite)")),
    }

    let event = calendar
        .components
        .iter()
        .find_map(|c| match c {
            CalendarComponent::Event(e) => Some(e),
            _ => None,
        })
        .ok_or_else(|| anyhow!("Sidecar .ics has no VEVENT"))?;

    let uid = event
        .get_uid()
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow!("Invite has no UID; cannot build a matching REPLY"))?;
    let sequence = event.get_sequence().unwrap_or(0);
    let summary = event.get_summary().map(|s| s.to_string());
    let organizer = event
        .property_value("ORGANIZER")
        .map(strip_mailto)
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow!("Invite has no ORGANIZER; cannot address the REPLY"))?;

    Ok(ReplyContext {
        uid,
        sequence,
        summary,
        organizer,
    })
}

/// Build a `METHOD:REPLY` `VCALENDAR` for an RSVP.
///
/// Copies `UID` + `SEQUENCE` verbatim from the source invite (`ctx`), sets a
/// single `ATTENDEE` = the responding account's address with the chosen
/// `PARTSTAT`, and a fresh `DTSTAMP`. `RECURRENCE-ID` is intentionally absent
/// (v1 answers the whole series only, D6). No `RSVP` parameter on a REPLY
/// attendee (that lives on the REQUEST). Renders to `.ics` bytes routed to the
/// `ORGANIZER`.
pub fn build_reply_ics(ctx: &ReplyContext, attendee: &str, rsvp: Rsvp) -> Result<String> {
    if attendee.trim().is_empty() {
        return Err(anyhow!("REPLY attendee address is empty"));
    }

    let mut event = Event::new();
    event
        .uid(&ctx.uid)
        .sequence(ctx.sequence)
        .timestamp(Utc::now()); // DTSTAMP
    if let Some(summary) = ctx.summary.as_deref().filter(|s| !s.trim().is_empty()) {
        event.summary(summary);
    }

    // ORGANIZER echoed back so the reply matches the request thread.
    event.append_property(Property::new(
        "ORGANIZER",
        format!("mailto:{}", ctx.organizer),
    ));

    // The single ATTENDEE line is the RSVP payload: our address + PARTSTAT.
    let mut prop = Property::new("ATTENDEE", format!("mailto:{}", attendee.trim()));
    prop.add_parameter("PARTSTAT", rsvp.partstat());
    event.append_property(prop);

    let mut calendar = Calendar::new();
    calendar.push(event.done());
    calendar.append_property(Property::new("METHOD", "REPLY"));

    Ok(calendar.to_string())
}

/// Convenience: resolve `--start` + (`--end` | `--duration`) into an absolute
/// `(start, end)` UTC pair with validation.
pub fn resolve_times(
    start: &str,
    end: Option<&str>,
    duration: Option<&str>,
) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let start_dt = parse_datetime(start).context("Invalid --start")?;
    let end_dt = match (end, duration) {
        (Some(_), Some(_)) => {
            return Err(anyhow!("Provide exactly one of --end or --duration, not both"))
        }
        (Some(e), None) => parse_datetime(e).context("Invalid --end")?,
        (None, Some(d)) => start_dt + parse_duration(d).context("Invalid --duration")?,
        (None, None) => return Err(anyhow!("An invite needs --end or --duration")),
    };
    if end_dt <= start_dt {
        return Err(anyhow!("--end must be after --start"));
    }
    Ok((start_dt, end_dt))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> InviteSpec {
        InviteSpec {
            uid: "fixed-uid@mailypoppins".to_string(),
            organizer: "me@example.com".to_string(),
            attendees: vec!["a@example.com".to_string(), "b@example.com".to_string()],
            summary: "LOC Day planning".to_string(),
            start: Utc.with_ymd_and_hms(2026, 7, 20, 12, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 7, 20, 13, 0, 0).unwrap(),
            location: Some("Room 4.12".to_string()),
            description: None,
        }
    }

    #[test]
    fn generate_uid_uses_sender_domain_and_is_unique() {
        let uid = generate_uid("me@example.com");
        assert!(uid.ends_with("@example.com"), "uid={}", uid);
        // 20 bytes -> 40 hex chars before the @domain.
        let (hex, domain) = uid.split_once('@').unwrap();
        assert_eq!(hex.len(), 40, "uid={}", uid);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(domain, "example.com");

        // Two calls must not collide.
        assert_ne!(generate_uid("me@example.com"), generate_uid("me@example.com"));

        // No `@domain` in the organizer -> fixed fallback suffix.
        let fallback = generate_uid("nobody");
        assert!(fallback.ends_with("@mailypoppins"), "uid={}", fallback);
    }

    #[test]
    fn parse_datetime_rfc3339_offset() {
        let dt = parse_datetime("2026-07-20T14:00:00+02:00").unwrap();
        assert_eq!(dt, Utc.with_ymd_and_hms(2026, 7, 20, 12, 0, 0).unwrap());
    }

    #[test]
    fn parse_datetime_utc_z() {
        let dt = parse_datetime("2026-07-20T12:00:00Z").unwrap();
        assert_eq!(dt, Utc.with_ymd_and_hms(2026, 7, 20, 12, 0, 0).unwrap());
    }

    #[test]
    fn parse_datetime_local_variants_parse() {
        // Exact instant depends on the machine zone; assert only that they parse.
        assert!(parse_datetime("2026-07-20T14:00:00").is_ok());
        assert!(parse_datetime("2026-07-20T14:00").is_ok());
        assert!(parse_datetime("2026-07-20 14:00").is_ok());
        assert!(parse_datetime("not a date").is_err());
    }

    #[test]
    fn parse_duration_iso_and_short() {
        assert_eq!(parse_duration("PT1H").unwrap(), Duration::hours(1));
        assert_eq!(parse_duration("PT30M").unwrap(), Duration::minutes(30));
        assert_eq!(parse_duration("PT1H30M").unwrap(), Duration::minutes(90));
        assert_eq!(parse_duration("P1DT2H").unwrap(), Duration::hours(26));
        assert_eq!(parse_duration("1h").unwrap(), Duration::hours(1));
        assert_eq!(parse_duration("90m").unwrap(), Duration::minutes(90));
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::minutes(90));
        assert_eq!(parse_duration("2d").unwrap(), Duration::days(2));
        assert!(parse_duration("PT0S").is_err()); // non-positive
        assert!(parse_duration("garbage").is_err());
        assert!(parse_duration("10").is_err()); // no unit
    }

    #[test]
    fn resolve_times_end_and_duration() {
        let (s, e) = resolve_times("2026-07-20T12:00:00Z", Some("2026-07-20T13:00:00Z"), None).unwrap();
        assert_eq!(e - s, Duration::hours(1));
        let (s, e) = resolve_times("2026-07-20T12:00:00Z", None, Some("PT90M")).unwrap();
        assert_eq!(e - s, Duration::minutes(90));
    }

    #[test]
    fn resolve_times_rejects_bad_combos() {
        assert!(resolve_times("2026-07-20T12:00:00Z", None, None).is_err());
        assert!(resolve_times(
            "2026-07-20T12:00:00Z",
            Some("2026-07-20T13:00:00Z"),
            Some("PT1H")
        )
        .is_err());
        // end before start
        assert!(resolve_times("2026-07-20T12:00:00Z", Some("2026-07-20T11:00:00Z"), None).is_err());
    }

    /// Undo RFC 5545 line folding (`CRLF` + a leading space/tab) so property
    /// values that the `icalendar` serializer splits across 75-octet lines can
    /// be asserted as contiguous substrings.
    fn unfold(ics: &str) -> String {
        ics.replace("\r\n ", "").replace("\r\n\t", "")
    }

    #[test]
    fn build_invite_ics_has_required_properties() {
        let ics = unfold(&build_invite_ics(&spec()).unwrap());
        assert!(ics.contains("BEGIN:VCALENDAR"));
        assert!(ics.contains("METHOD:REQUEST"));
        assert!(ics.contains("BEGIN:VEVENT"));
        assert!(ics.contains("UID:fixed-uid@mailypoppins"));
        assert!(ics.contains("SEQUENCE:0"));
        assert!(ics.contains("DTSTAMP:"));
        assert!(ics.contains("SUMMARY:LOC Day planning"));
        assert!(ics.contains("DTSTART:20260720T120000Z"));
        assert!(ics.contains("DTEND:20260720T130000Z"));
        assert!(ics.contains("LOCATION:Room 4.12"));
        assert!(ics.contains("ORGANIZER:mailto:me@example.com"));
        assert!(ics.contains("mailto:a@example.com"));
        assert!(ics.contains("mailto:b@example.com"));
        assert!(ics.contains("RSVP=TRUE"));
        assert!(ics.contains("PARTSTAT=NEEDS-ACTION"));
        // Exactly one PRODID (a duplicate would be invalid RFC 5545).
        assert_eq!(ics.matches("PRODID:").count(), 1, "ics=\n{}", ics);
    }

    #[test]
    fn build_invite_ics_validates() {
        let mut s = spec();
        s.attendees.clear();
        assert!(build_invite_ics(&s).is_err(), "no attendees");

        let mut s = spec();
        s.end = s.start;
        assert!(build_invite_ics(&s).is_err(), "end == start");

        let mut s = spec();
        s.summary = "   ".to_string();
        assert!(build_invite_ics(&s).is_err(), "empty summary");
    }

    #[test]
    fn build_invite_ics_escapes_text_per_rfc5545() {
        // Assert the icalendar crate performs RFC 5545 TEXT escaping so we do
        // not have to (comma, semicolon, newline in SUMMARY/LOCATION).
        let mut s = spec();
        s.summary = "Plan A, B; C\nnext line".to_string();
        s.location = Some("Room, 4;12".to_string());
        let ics = unfold(&build_invite_ics(&s).unwrap());
        assert!(
            ics.contains(r"SUMMARY:Plan A\, B\; C\nnext line"),
            "escaped SUMMARY missing in:\n{}",
            ics
        );
        assert!(
            ics.contains(r"LOCATION:Room\, 4\;12"),
            "escaped LOCATION missing in:\n{}",
            ics
        );
        // Raw (unescaped) forms must NOT appear.
        assert!(!ics.contains("SUMMARY:Plan A, B; C"));
    }

    // A received Outlook-style REQUEST used as the RSVP source of truth.
    const RECEIVED_REQUEST: &str = "\
BEGIN:VCALENDAR\r
PRODID:-//Microsoft Corporation//Outlook 16.0 MIMEDIR//EN\r
VERSION:2.0\r
METHOD:REQUEST\r
BEGIN:VEVENT\r
UID:040000008200E00074C5B7101A82E00800000000@outlook.com\r
SEQUENCE:3\r
SUMMARY:LOC Day planning\r
DTSTART:20260720T120000Z\r
DTEND:20260720T130000Z\r
ORGANIZER;CN=Chair:mailto:chair@tum.de\r
ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:me@example.com\r
END:VEVENT\r
END:VCALENDAR\r
";

    #[test]
    fn reply_context_extracts_uid_sequence_organizer_from_sidecar() {
        let ctx = reply_context_from_ics(RECEIVED_REQUEST.as_bytes()).unwrap();
        assert_eq!(ctx.uid, "040000008200E00074C5B7101A82E00800000000@outlook.com");
        assert_eq!(ctx.sequence, 3);
        assert_eq!(ctx.summary.as_deref(), Some("LOC Day planning"));
        assert_eq!(ctx.organizer, "chair@tum.de");
    }

    #[test]
    fn reply_context_rejects_non_request_methods() {
        let reply = RECEIVED_REQUEST.replace("METHOD:REQUEST", "METHOD:REPLY");
        assert!(reply_context_from_ics(reply.as_bytes()).is_err());
        let cancel = RECEIVED_REQUEST.replace("METHOD:REQUEST", "METHOD:CANCEL");
        assert!(reply_context_from_ics(cancel.as_bytes()).is_err());
        assert!(reply_context_from_ics(b"not a calendar").is_err());
    }

    #[test]
    fn build_reply_ics_copies_uid_sequence_and_sets_partstat() {
        let ctx = reply_context_from_ics(RECEIVED_REQUEST.as_bytes()).unwrap();
        for (rsvp, partstat) in [
            (Rsvp::Accepted, "ACCEPTED"),
            (Rsvp::Tentative, "TENTATIVE"),
            (Rsvp::Declined, "DECLINED"),
        ] {
            let ics = unfold(&build_reply_ics(&ctx, "me@example.com", rsvp).unwrap());
            // METHOD:REPLY at VCALENDAR level.
            assert!(ics.contains("METHOD:REPLY"), "ics=\n{}", ics);
            // UID + SEQUENCE copied verbatim from the sidecar (not regenerated).
            assert!(ics.contains("UID:040000008200E00074C5B7101A82E00800000000@outlook.com"));
            assert!(ics.contains("SEQUENCE:3"));
            // Fresh DTSTAMP present.
            assert!(ics.contains("DTSTAMP:"));
            // Exactly one ATTENDEE = our address, with the chosen PARTSTAT.
            assert_eq!(ics.matches("ATTENDEE").count(), 1, "ics=\n{}", ics);
            assert!(ics.contains("mailto:me@example.com"));
            assert!(
                ics.contains(&format!("PARTSTAT={}", partstat)),
                "missing PARTSTAT={} in\n{}",
                partstat,
                ics
            );
            // ORGANIZER echoed so the organizer's client threads it correctly.
            assert!(ics.contains("ORGANIZER:mailto:chair@tum.de"), "ics=\n{}", ics);
            // Whole-series only: no per-occurrence RECURRENCE-ID (D6).
            assert!(!ics.contains("RECURRENCE-ID"), "ics=\n{}", ics);
            // A REPLY attendee carries no RSVP parameter (that lives on REQUEST).
            assert!(!ics.contains("RSVP=TRUE"), "ics=\n{}", ics);
        }
    }

    #[test]
    fn build_reply_ics_roundtrips_through_receive_parser() {
        let ctx = reply_context_from_ics(RECEIVED_REQUEST.as_bytes()).unwrap();
        let ics = build_reply_ics(&ctx, "me@example.com", Rsvp::Accepted).unwrap();
        let parsed = crate::calendar::parse_ics(ics.as_bytes()).expect("parses");
        assert_eq!(parsed.method.as_deref(), Some("REPLY"));
        assert_eq!(
            parsed.uid.as_deref(),
            Some("040000008200E00074C5B7101A82E00800000000@outlook.com")
        );
        assert_eq!(parsed.sequence, 3);
        assert_eq!(parsed.organizer.as_deref(), Some("chair@tum.de"));
        assert_eq!(parsed.attendees.len(), 1);
        assert_eq!(parsed.attendees[0].address, "me@example.com");
        assert_eq!(parsed.attendees[0].status, "accepted");
    }

    #[test]
    fn build_invite_ics_roundtrips_through_receive_parser() {
        // The sent invite must parse back through the #0027 receive path.
        let ics = build_invite_ics(&spec()).unwrap();
        assert!(crate::calendar::is_imip_invite(ics.as_bytes()));
        let parsed = crate::calendar::parse_ics(ics.as_bytes()).expect("parses");
        assert_eq!(parsed.method.as_deref(), Some("REQUEST"));
        assert_eq!(parsed.uid.as_deref(), Some("fixed-uid@mailypoppins"));
        assert_eq!(parsed.sequence, 0);
        assert_eq!(parsed.summary.as_deref(), Some("LOC Day planning"));
        assert_eq!(parsed.start.as_deref(), Some("2026-07-20T12:00:00Z"));
        assert_eq!(parsed.end.as_deref(), Some("2026-07-20T13:00:00Z"));
        assert_eq!(parsed.location.as_deref(), Some("Room 4.12"));
        assert_eq!(parsed.organizer.as_deref(), Some("me@example.com"));
        assert_eq!(parsed.attendees.len(), 2);
        assert_eq!(parsed.attendees[0].address, "a@example.com");
    }
}

