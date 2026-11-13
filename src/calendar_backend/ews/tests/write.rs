//! The writes (`write.rs`) through the scripted seam: every envelope the
//! server receives, asserted whole, and the answer read.
//!
//! The answers are built from Microsoft's schema documentation
//! ([`built_answer`]), since the spike ran no write.

use chrono::{TimeZone, Utc};

use super::super::fetch::envelope;
use super::super::EwsCalendarBackend;
use super::parse::{OCCURRENCE_ID, SINGLE_ID};
use super::{built_answer, error_message, success_message, ScriptedFetch};
use crate::calendar_backend::{CalendarBackend, Response};
use crate::calendar_model::TimeSlot;
use crate::calendar_write::ProposedTime;
use crate::config::EwsPasswordSource;

fn backend(fetch: ScriptedFetch) -> EwsCalendarBackend<ScriptedFetch> {
    EwsCalendarBackend::with_fetch("tum", EwsPasswordSource::Imap, fetch)
}

/// What the server answers a response object made through `CreateItem`
/// with: one successful message, its `Items` empty since a sent response is
/// not kept in the calendar.
fn response_made() -> String {
    built_answer("CreateItem", &success_message("CreateItem", "<m:Items/>"))
}

/// 14:00 to 15:00 in Berlin on 20 November 2026, typed in Berlin.
fn proposal() -> ProposedTime {
    ProposedTime {
        slot: TimeSlot {
            start: Utc.with_ymd_and_hms(2026, 11, 20, 13, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 11, 20, 14, 0, 0).unwrap(),
        },
        zone: "Europe/Berlin".into(),
    }
}

/// An accept that tells the organizer is an `AcceptItem` sent and saved,
/// on the event's ItemId, the comment its plain-text body with the user's
/// markup escaped.
#[tokio::test]
async fn an_accept_sends_the_response_with_the_comment_escaped() {
    let backend = backend(ScriptedFetch::default().answer("CreateItem", &response_made()));
    backend
        .respond(
            SINGLE_ID,
            Response::Accept,
            Some("See you <there> & then".into()),
            true,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        backend.fetch().sent(),
        [envelope(&format!(
            "<m:CreateItem MessageDisposition=\"SendAndSaveCopy\"><m:Items><t:AcceptItem>\
             <t:Body BodyType=\"Text\">See you &lt;there&gt; &amp; then</t:Body>\
             <t:ReferenceItemId Id=\"{SINGLE_ID}\"/></t:AcceptItem></m:Items></m:CreateItem>"
        ))]
    );
}

/// A tentative that tells nobody is saved only, an empty comment is no body,
/// and the proposed time goes out as its two UTC instants.
#[tokio::test]
async fn a_tentative_saved_only_carries_the_proposed_time() {
    let backend = backend(ScriptedFetch::default().answer("CreateItem", &response_made()));
    backend
        .respond(
            OCCURRENCE_ID,
            Response::Tentative,
            Some("  ".into()),
            false,
            Some(proposal()),
        )
        .await
        .unwrap();
    assert_eq!(
        backend.fetch().sent(),
        [envelope(&format!(
            "<m:CreateItem MessageDisposition=\"SaveOnly\"><m:Items><t:TentativelyAcceptItem>\
             <t:ReferenceItemId Id=\"{OCCURRENCE_ID}\"/>\
             <t:ProposedStart>2026-11-20T13:00:00Z</t:ProposedStart>\
             <t:ProposedEnd>2026-11-20T14:00:00Z</t:ProposedEnd>\
             </t:TentativelyAcceptItem></m:Items></m:CreateItem>"
        ))]
    );
}

/// A decline carries its comment before the reference and the proposal
/// after it, the schema's order; an accept never carries a proposal.
#[tokio::test]
async fn a_decline_proposes_and_an_accept_never_does() {
    let backend = backend(
        ScriptedFetch::default()
            .answer("CreateItem", &response_made())
            .answer("CreateItem", &response_made()),
    );
    backend
        .respond(
            SINGLE_ID,
            Response::Decline,
            Some("Clash".into()),
            true,
            Some(proposal()),
        )
        .await
        .unwrap();
    backend
        .respond(SINGLE_ID, Response::Accept, None, true, Some(proposal()))
        .await
        .unwrap();
    let sent = backend.fetch().sent();
    assert_eq!(
        sent[0],
        envelope(&format!(
            "<m:CreateItem MessageDisposition=\"SendAndSaveCopy\"><m:Items><t:DeclineItem>\
             <t:Body BodyType=\"Text\">Clash</t:Body><t:ReferenceItemId Id=\"{SINGLE_ID}\"/>\
             <t:ProposedStart>2026-11-20T13:00:00Z</t:ProposedStart>\
             <t:ProposedEnd>2026-11-20T14:00:00Z</t:ProposedEnd>\
             </t:DeclineItem></m:Items></m:CreateItem>"
        ))
    );
    assert_eq!(
        sent[1],
        envelope(&format!(
            "<m:CreateItem MessageDisposition=\"SendAndSaveCopy\"><m:Items><t:AcceptItem>\
             <t:ReferenceItemId Id=\"{SINGLE_ID}\"/></t:AcceptItem></m:Items></m:CreateItem>"
        ))
    );
}

/// A response the server refuses (an invitation the organizer cancelled) is
/// the server's code and sentence, naming the operation.
#[tokio::test]
async fn a_refused_response_is_the_servers_sentence() {
    let refused = built_answer(
        "CreateItem",
        &error_message(
            "CreateItem",
            "ErrorCalendarIsCancelledForAccept",
            "Calendar item is canceled and can&apos;t be accepted.",
        ),
    );
    let backend = backend(ScriptedFetch::default().answer("CreateItem", &refused));
    let err = backend
        .respond(SINGLE_ID, Response::Accept, None, true, None)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "the Exchange server refused CreateItem: ErrorCalendarIsCancelledForAccept: Calendar \
         item is canceled and can't be accepted."
    );
}
