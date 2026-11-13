//! The send-side half of iMIP that reads an account (#0126, P5-U10a).
//!
//! The ICS building, the RSVP reply and the date/duration grammar are
//! [`mp_core::invite`] and are re-exported here, so `crate::invite::Rsvp`,
//! `crate::invite::build_invite_ics` and the rest resolve as they did when
//! this file held all of it. What stayed is the one function that needs an
//! account: `plan_invite` reads the account's auth method to refuse Graph, its
//! `default_from` for the `ORGANIZER`, and `send::split_addresses` for the
//! attendee list. Beside it, the organizer's changes to an invitation already
//! sent (PERSO-204): the sent copy read back out of the store, the messages
//! an update or a cancellation sends, and the iMIP message every one of them
//! (the first invitation included) is built into.

use anyhow::{anyhow, Result};

use crate::calendar_model::EventTime;
use crate::calendar_write::EventPatch;
use crate::types::{EmailDraft, EmailFrontmatter, EmailStatus};

pub use mp_core::invite::*;

/// One sentence, refused in two places: the client, which must not preview an
/// invitation it is going to refuse, and `send.invite`, which refuses it before
/// it looks at anything else so a GUI can disable the action with its reason.
pub const GRAPH_REFUSAL: &str = "`mp send --invite` is not supported for Graph accounts yet \
                                (Graph calendar send is tracked by #0036, blocked on #0035). \
                                Use an SMTP-configured account.";

/// The flags `mp send --invite` takes, unresolved.
#[derive(Debug, Clone, Default)]
pub struct InviteRequest {
    pub to: Option<String>,
    pub cc: Option<String>,
    pub subject: Option<String>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub duration: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    /// `SEQUENCE`, `None` for a new invitation (0). An update of one already
    /// sent passes that invitation's UID to [`plan_invite`] and a higher
    /// sequence here.
    pub sequence: Option<u32>,
}

/// One validated invitation: the preview the client renders and the `VEVENT`
/// the daemon builds, from one resolution of the same flags.
#[derive(Debug, Clone)]
pub struct InvitePlan {
    /// The event `SUMMARY`, which is also the message subject.
    pub subject: String,
    /// The `To:` header, exactly as it was typed.
    pub to_field: Option<String>,
    /// The `Cc:` header, likewise.
    pub cc_field: Option<String>,
    /// The spec the ICS is built from.
    pub spec: InviteSpec,
    /// The human-readable body: the description, or a one-line summary.
    pub body: String,
}

/// Validate one invitation request against the account that would send it.
///
/// The order of the refusals is the contract, because it is the order the user
/// has always met them in: the account first (`ANO-4`, decided before anything
/// else about the invitation is looked at), then the subject, the start, the
/// times, the recipients and the organizer.
///
/// `uid` is the client's when it has already previewed one, so the UID the user
/// read is the UID that goes out, or the earlier invitation's when the user is
/// sending an update (`mp send --invite --uid`), and minted here otherwise.
pub fn plan_invite(
    account: &crate::config::AccountConfig,
    request: &InviteRequest,
    uid: Option<&str>,
) -> Result<InvitePlan> {
    if account.auth_method == crate::config::AuthMethod::Graph {
        return Err(anyhow!("{GRAPH_REFUSAL}"));
    }
    let subject = request
        .subject
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("--invite requires --subject (used as the event summary)"))?;
    let start = request
        .start
        .as_deref()
        .ok_or_else(|| anyhow!("--invite requires --start"))?;
    let span = resolve_span(start, request.end.as_deref(), request.duration.as_deref())?;

    // Attendees from --to/--cc (deduplicated, bare addresses). The To/Cc
    // headers keep the full form; ATTENDEE lines take the extracted address.
    let to_field = request.to.as_deref().filter(|s| !s.trim().is_empty());
    let cc_field = request.cc.as_deref().filter(|s| !s.trim().is_empty());
    let mut attendees: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for field in [to_field, cc_field].into_iter().flatten() {
        for raw in crate::send::split_addresses(field) {
            let address = crate::parse::extract_email_address(&raw);
            if !address.is_empty() && seen.insert(address.to_lowercase()) {
                attendees.push(address);
            }
        }
    }
    if attendees.is_empty() {
        return Err(anyhow!(
            "--invite requires at least one recipient via --to/--cc"
        ));
    }

    // ORGANIZER = the sending account's primary address (it must match, or
    // Exchange silently drops the invitation).
    let organizer = crate::parse::extract_email_address(&account.default_from);
    if organizer.is_empty() {
        return Err(anyhow!(
            "Account has no usable primary address (default_from); cannot set ORGANIZER"
        ));
    }

    // A UID the caller names is written into the VEVENT as it is: it has to be
    // one line of text, or the update would name a different event.
    let uid = match uid.map(str::trim) {
        Some("") => return Err(anyhow!("--uid is empty")),
        Some(u) if u.chars().any(char::is_control) => {
            return Err(anyhow!("--uid must be a single line of text"))
        }
        Some(u) => u.to_string(),
        None => generate_uid(&organizer),
    };

    let body = request
        .description
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("You are invited to: {subject}"));

    Ok(InvitePlan {
        subject: subject.to_string(),
        to_field: to_field.map(str::to_string),
        cc_field: cc_field.map(str::to_string),
        spec: InviteSpec {
            uid,
            sequence: request.sequence.unwrap_or(0),
            organizer,
            attendees,
            summary: subject.to_string(),
            span,
            location: request.location.clone().filter(|s| !s.trim().is_empty()),
            description: request.description.clone().filter(|s| !s.trim().is_empty()),
        },
        body,
    })
}

/// One iTIP message an organizer sends: an invitation, an update or a
/// cancellation, before it is built into MIME by [`itip_built`].
#[derive(Debug, Clone)]
pub struct ItipMessage {
    pub subject: String,
    /// The `To:` header, as typed for a first invitation, the attendees
    /// joined for a change.
    pub to: Option<String>,
    pub cc: Option<String>,
    /// The human-readable body.
    pub body: String,
    /// The `VCALENDAR`, whose `METHOD` names the MIME part's `method`.
    pub ics: String,
}

/// The iMIP message `message` is, from `account`, built by the one MIME
/// builder every send uses: `multipart/mixed` around the text, the HTML and
/// the inline `text/calendar` part, with the same bytes as `invite.ics`.
///
/// The draft it goes through is synthetic and never written: the Sent copy
/// is the outbox's (#0037).
pub fn itip_built(
    account: &crate::config::AccountConfig,
    default_from: &str,
    email: &crate::config::EmailSettings,
    signature: Option<&str>,
    message: &ItipMessage,
) -> Result<crate::send::BuiltMessage> {
    let event = crate::calendar::parse_ics(message.ics.as_bytes())
        .map(|p| crate::calendar::event_frontmatter(&p));
    let draft = EmailDraft {
        path: std::path::PathBuf::from(format!(
            "{}-invite-{}.md",
            chrono::Utc::now().format("%Y%m%d-%H%M%S"),
            crate::parse::slugify_subject(&message.subject)
        )),
        frontmatter: EmailFrontmatter {
            id: None,
            date: None,
            to: message.to.clone(),
            cc: message.cc.clone(),
            bcc: None,
            subject: message.subject.clone(),
            status: EmailStatus::Approved,
            from: Some(account.default_from.clone()),
            reply_to: None,
            attachments: None,
            sent_at: None,
            sent_via: None,
            message_id: None,
            in_reply_to: None,
            forwarded_from: None,
            signature: None,
            event,
            server_message_id: None,
            conflict_of: None,
        },
        body_markdown: message.body.clone(),
    };
    // An invitation is a fresh message, never a reply: no parent to thread
    // under.
    crate::send::build_draft_message(
        &draft,
        default_from,
        email,
        signature,
        Some(&message.ics),
        None,
    )
}

/// An invitation the account organised, as one stored row holds it, and the
/// `SEQUENCE` its next version goes out with.
#[derive(Debug, Clone)]
pub struct OrganizedRow {
    pub invite: OrganizedInvite,
    /// Above every REQUEST and CANCEL of the `UID` the account holds
    /// ([`crate::reconcile::next_sequence`]).
    pub next_sequence: u32,
    /// The `RECURRENCE-ID`s the account's cancellations of this version or
    /// a later one name, the occurrences of the series cancelled already,
    /// in [`crate::calendar::ParsedEvent::recurrence_id`]'s form.
    pub cancelled_occurrences: Vec<String>,
}

/// The invitation on row `row_id` of `account`'s store, refused unless the
/// account organised it: its `ORGANIZER` is the account's primary address,
/// the address an update or a cancellation goes out from (Exchange drops a
/// mismatch), and the row is the account's own sent copy, as the agenda's
/// `is_organizer` asks ([`crate::reconcile::is_sent_copy`]): a REQUEST that
/// names the account as its organizer and arrived in the inbox is one
/// anyone could forge, and changing it would mail its attendees from the
/// account.
pub fn organized_row(
    store: &crate::store::Store,
    blobs: &crate::store::BlobStore,
    account: &crate::config::AccountConfig,
    row_id: i64,
) -> Result<OrganizedRow> {
    let ics = crate::store::read::load_invite_ics(store, blobs, row_id).ok_or_else(|| {
        anyhow!(
            "Row {row_id} of {} carries no invitation to change",
            account.name
        )
    })?;
    let invite = organized_from_ics(&ics)?;
    let own = crate::parse::extract_email_address(&account.default_from);
    if !invite.organizer.eq_ignore_ascii_case(&own) {
        return Err(anyhow!(
            "{} was organised by {}, not by {}: only its organizer changes or cancels it, \
             and an invitation you were sent can only be answered (event.respond)",
            invite.title(),
            invite.organizer,
            account.name
        ));
    }
    let mailbox = crate::store::read::find_by_id(store, row_id)?
        .map(|row| row.mailbox)
        .unwrap_or_default();
    if !crate::reconcile::is_sent_copy(&mailbox) {
        return Err(anyhow!(
            "{} names {} as its organizer but was received in {mailbox}, not sent from it: mp \
             changes or cancels only the sent copy of an invitation the account organised, and \
             an invitation you were sent can only be answered (event.respond)",
            invite.title(),
            account.name
        ));
    }
    let held = crate::reconcile::load_invites(store, blobs, &account.name);
    let next_sequence = crate::reconcile::next_sequence(&held, &invite.uid, invite.sequence);
    let cancelled_occurrences = held
        .iter()
        .filter(|held| held.uid() == Some(invite.uid.as_str()) && held.method() == "CANCEL")
        .filter(|held| held.parsed.sequence >= invite.sequence)
        .filter_map(|held| held.recurrence_id().map(str::to_string))
        .collect();
    Ok(OrganizedRow {
        invite,
        next_sequence,
        cancelled_occurrences,
    })
}

/// The messages an update of `invite` sends, in the order they go: the
/// update to every attendee it invites, then, when it drops attendees, a
/// cancellation to those.
///
/// The update goes first because it is the change: one that can reach
/// nobody must cancel nobody, while one the outbox still holds tells the
/// dropped attendees at once. The cancellation is marked as their
/// uninvitation ([`build_uninvite_ics`]), which [`crate::reconcile`] reads
/// on the organizer's own agenda, so it never ends the meeting there, even
/// while the update waits in the outbox and the agenda holds the version
/// it names every attendee of. The cancellation takes
/// `next_sequence` and the update the one above it, so a dropped attendee's
/// copy, at most at `next_sequence - 1`, is cancelled by it, while on the
/// organizer's agenda it is a stale CANCEL below the update's sequence.
pub fn update_messages(
    invite: &OrganizedInvite,
    current: &EventTime,
    patch: &EventPatch,
    next_sequence: u32,
) -> Result<(InviteSpec, Vec<ItipMessage>), String> {
    let InviteUpdate { mut spec, removed } = updated_invite(invite, current, patch, next_sequence)?;
    let dropped = if removed.is_empty() {
        None
    } else {
        spec.sequence = next_sequence.saturating_add(1);
        let ics =
            build_uninvite_ics(invite, next_sequence, &removed).map_err(|e| format!("{e:#}"))?;
        Some(cancellation(invite.title(), &removed, ics, None, None))
    };
    let ics = build_invite_ics(&spec).map_err(|e| format!("{e:#}"))?;
    let body = spec
        .description
        .clone()
        .unwrap_or_else(|| format!("The invitation to {} was updated.", spec.summary));
    let mut messages = vec![ItipMessage {
        subject: spec.summary.clone(),
        to: Some(spec.attendees.join(", ")),
        cc: None,
        body,
        ics,
    }];
    messages.extend(dropped);
    Ok((spec, messages))
}

/// The cancellation of `invite` at `sequence`, told to every attendee: of
/// the whole event, or of the occurrence `occurrence` names, with the
/// organizer's `comment` after the body's first line.
pub fn cancel_message(
    invite: &OrganizedInvite,
    sequence: u32,
    occurrence: Option<&icalendar::Property>,
    comment: Option<&str>,
) -> Result<ItipMessage> {
    let ics = build_cancel_ics(invite, sequence, occurrence, &invite.attendees)?;
    // The occurrence as the receive side names it, which is how a person
    // reads it too (`2026-12-21T09:00:00+01:00`).
    let when = match occurrence {
        Some(_) => crate::calendar::parse_ics(ics.as_bytes()).and_then(|p| p.recurrence_id),
        None => None,
    };
    Ok(cancellation(
        invite.title(),
        &invite.attendees,
        ics,
        when.as_deref(),
        comment,
    ))
}

/// A cancellation message: `Cancelled: <summary>`, the occurrence named on
/// the body's first line when it is one, then the comment.
fn cancellation(
    title: &str,
    attendees: &[String],
    ics: String,
    occurrence: Option<&str>,
    comment: Option<&str>,
) -> ItipMessage {
    let mut body = match occurrence {
        Some(when) => format!("Cancelled: {title}, the occurrence of {when}"),
        None => format!("Cancelled: {title}"),
    };
    if let Some(comment) = comment.map(str::trim).filter(|c| !c.is_empty()) {
        body.push_str("\n\n");
        body.push_str(comment);
    }
    ItipMessage {
        subject: format!("Cancelled: {title}"),
        to: Some(attendees.join(", ")),
        cc: None,
        body,
        ics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> crate::config::AccountConfig {
        crate::config::AccountConfig {
            name: "acct".to_string(),
            default_from: "Me <me@example.com>".to_string(),
            ..Default::default()
        }
    }

    fn request(start: &str) -> InviteRequest {
        InviteRequest {
            to: Some("a@example.com".to_string()),
            subject: Some("Kita closed".to_string()),
            start: Some(start.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn a_new_invitation_mints_a_uid_at_sequence_zero() {
        let plan = plan_invite(&account(), &request("2026-12-24"), None).unwrap();
        assert!(plan.spec.uid.ends_with("@example.com"), "{}", plan.spec.uid);
        assert_eq!(plan.spec.sequence, 0);
        assert!(matches!(plan.spec.span, EventSpan::AllDay { .. }));
    }

    #[test]
    fn an_update_keeps_the_named_uid_and_sequence() {
        let req = InviteRequest {
            sequence: Some(2),
            ..request("2026-12-24")
        };
        let plan = plan_invite(&account(), &req, Some(" earlier@example.com ")).unwrap();
        assert_eq!(plan.spec.uid, "earlier@example.com");
        assert_eq!(plan.spec.sequence, 2);
    }

    #[test]
    fn a_named_uid_must_be_one_non_empty_line() {
        let req = request("2026-12-24");
        let err = |uid: &str| {
            format!(
                "{:#}",
                plan_invite(&account(), &req, Some(uid)).unwrap_err()
            )
        };
        assert_eq!(err("  "), "--uid is empty");
        assert_eq!(err("a\r\nb"), "--uid must be a single line of text");
    }

    #[test]
    fn a_timed_invitation_still_needs_an_end() {
        let err = plan_invite(&account(), &request("2026-12-24T10:00"), None).unwrap_err();
        assert_eq!(format!("{err:#}"), "An invite needs --end or --duration");
    }
}
